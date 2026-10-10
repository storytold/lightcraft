//! Upstream tracking (plan/upstream.md, docs/upstream-merge.md):
//!
//! - `cargo xtask upstream-merge [--ref REF] [--no-ci]`: merge upstream on `merge/upstream-YYYYMMDD`, keep our
//!   version of every owned path (`upstream-owned.txt`), write a review report of what upstream changed there, map
//!   crate names, report brand findings, then run `ci`.
//! - `cargo xtask upstream-pr <branch> <commit>…`: replay our commits onto a branch off `upstream/main` with our
//!   crate names mapped back to upstream's. Never pushes.
//! - `cargo xtask shared-check [--strict]`: commits since the last upstream merge that change shared paths without
//!   an `UPSTREAM-PR:` line.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::rename::UPSTREAM_PREFIX;

pub const MANIFEST: &str = "upstream-owned.txt";
const DEFAULT_REF: &str = "upstream/main";
const FORK_BASE: &str = "fork-base";
/// Listed offenders in `ci` (warn mode); `cargo xtask shared-check` prints all.
const CI_LIST_LIMIT: usize = 25;

// ---------------------------------------------------------------- manifest + path globs

/// One gitignore-style path glob.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    segs: Vec<String>,
    /// contains `/`: matched from the repo root; otherwise against the file name at any depth
    anchored: bool,
    /// trailing `/`: matches everything below the directory
    dir: bool,
}

impl Pattern {
    pub fn parse(s: &str) -> Option<Pattern> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let anchored = s.contains('/');
        let dir = s.ends_with('/');
        let segs: Vec<String> = s.trim_start_matches('/').split('/').filter(|x| !x.is_empty()).map(str::to_string).collect();
        if segs.is_empty() {
            return None;
        }
        Some(Pattern { segs, anchored, dir })
    }

    /// `path` is a repo-relative file path with `/` separators.
    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').filter(|x| !x.is_empty()).collect();
        let pat: Vec<&str> = self.segs.iter().map(String::as_str).collect();
        if !self.anchored {
            // a bare name: the file name, or (dir pattern without `/` can't occur: it'd be anchored) any segment
            return parts.last().is_some_and(|name| glob_segment(pat.first().copied().unwrap_or(""), name));
        }
        if self.dir {
            // some directory above the file matches
            (1..parts.len()).any(|n| parts.get(..n).is_some_and(|prefix| match_segs(&pat, prefix)))
        } else {
            match_segs(&pat, &parts)
        }
    }
}

fn match_segs(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| path.get(skip..).is_some_and(|p| match_segs(rest, p))),
        Some((p, rest)) => match path.split_first() {
            Some((s, tail)) => glob_segment(p, s) && match_segs(rest, tail),
            None => false,
        },
    }
}

/// `*` and `?` within one segment (iterative, linear backtracking).
pub fn glob_segment(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(c) if *c == '?' || Some(c) == t.get(ti) => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p.get(pi..).is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}

#[derive(Debug, Default)]
pub struct Manifest {
    pub owned: Vec<Pattern>,
    pub regenerated: Vec<Pattern>,
    pub exempt: Vec<Pattern>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let mut m = Manifest::default();
        let mut section: Option<&str> = None;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                match name {
                    "owned" => section = Some("owned"),
                    "regenerated" => section = Some("regenerated"),
                    "exempt" => section = Some("exempt"),
                    other => return Err(format!("{MANIFEST}:{}: unknown section [{other}]", n + 1)),
                }
                continue;
            }
            let pat = Pattern::parse(line).ok_or_else(|| format!("{MANIFEST}:{}: bad pattern `{line}`", n + 1))?;
            match section {
                Some("owned") => m.owned.push(pat),
                Some("regenerated") => m.regenerated.push(pat),
                Some(_) => m.exempt.push(pat),
                None => return Err(format!("{MANIFEST}:{}: pattern before any [section]", n + 1)),
            }
        }
        Ok(m)
    }

    pub fn load(root: &Path) -> Result<Manifest, String> {
        let text = std::fs::read_to_string(root.join(MANIFEST)).map_err(|e| format!("{MANIFEST}: {e}"))?;
        Manifest::parse(&text)
    }

    pub fn is_owned(&self, path: &str) -> bool {
        self.owned.iter().any(|p| p.matches(path))
    }

    /// Code both sides share: not owned, not regenerated, not exempt.
    pub fn is_shared(&self, path: &str) -> bool {
        !self.is_owned(path) && !self.regenerated.iter().any(|p| p.matches(path)) && !self.exempt.iter().any(|p| p.matches(path))
    }
}

/// The commit message names the upstream PR the change was sent as (`UPSTREAM-PR: <link>`).
pub fn has_upstream_pr_tag(message: &str) -> bool {
    message.lines().any(|l| l.trim().strip_prefix("UPSTREAM-PR:").is_some_and(|rest| !rest.trim().is_empty()))
}

// ---------------------------------------------------------------- git helpers

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").current_dir(root).args(args).output().map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_ok(root: &Path, args: &[&str]) -> bool {
    Command::new("git").current_dir(root).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn lines(s: &str) -> Vec<String> {
    s.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
}

fn require_clean(root: &Path) -> Result<(), String> {
    let status = git(root, &["status", "--porcelain", "--untracked-files=no"])?;
    if !status.trim().is_empty() {
        return Err(format!("the working tree has uncommitted changes; commit or set them aside first:\n{status}"));
    }
    Ok(())
}

fn conflicted(root: &Path) -> Result<Vec<String>, String> {
    Ok(lines(&git(root, &["diff", "--name-only", "--diff-filter=U"])?))
}

/// `YYYYMMDD` (UTC) without a date crate.
fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}")
}

/// Days since 1970-01-01 to (year, month, day) (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

// ---------------------------------------------------------------- upstream-merge

pub fn merge(root: &Path, args: &[&str]) -> Result<(), String> {
    let usage = "usage: cargo xtask upstream-merge [--ref REF] [--no-ci]";
    let mut reference = DEFAULT_REF.to_string();
    let mut run_ci = true;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match *a {
            "--ref" => {
                i += 1;
                reference = args.get(i).ok_or(usage)?.to_string();
            }
            "--no-ci" => run_ci = false,
            other => return Err(format!("unknown argument `{other}`\n{usage}")),
        }
        i += 1;
    }
    let manifest = Manifest::load(root)?;
    require_clean(root)?;
    if !git_ok(root, &["config", "rerere.enabled"]) {
        eprintln!("hint: `git config rerere.enabled true` replays conflict resolutions in later merges");
    }

    let remote = reference.split('/').next().unwrap_or("upstream");
    if reference.contains('/') {
        eprintln!("$ git fetch {remote}");
        git(root, &["fetch", remote])?;
    }
    let date = today();
    let branch = format!("merge/upstream-{date}");
    git(root, &["switch", "-c", &branch])?;
    eprintln!("on branch {branch}");
    let base = git(root, &["merge-base", "HEAD", &reference])?.trim().to_string();

    // the merge itself; a conflict is a non-zero exit, which is expected here
    let merged = Command::new("git")
        .current_dir(root)
        .args(["-c", "merge.conflictStyle=zdiff3", "merge", "--no-ff", "--no-commit", &reference])
        .status()
        .map_err(|e| format!("git merge: {e}"))?;
    if !merged.success() && !git_ok(root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]) {
        return Err(format!("git merge {reference} failed before merging (see above)"));
    }

    // owned paths keep our version (merge=ours would only cover files both sides changed)
    let mut touched: BTreeSet<String> = BTreeSet::new();
    touched.extend(lines(&git(root, &["diff", "--name-only", "--no-renames", "--cached", "HEAD"])?));
    touched.extend(lines(&git(root, &["diff", "--name-only", "--no-renames", "HEAD"])?));
    touched.extend(conflicted(root)?);
    let (mut restored, mut removed) = (Vec::new(), Vec::new());
    for path in touched.iter().filter(|p| manifest.is_owned(p)) {
        if git_ok(root, &["cat-file", "-e", &format!("HEAD:{path}")]) {
            git(root, &["checkout", "HEAD", "--", path])?;
            restored.push(path.clone());
        } else {
            git(root, &["rm", "-q", "-f", "--ignore-unmatch", "--", path])?;
            let _ = std::fs::remove_file(root.join(path));
            removed.push(path.clone());
        }
    }
    println!("owned paths: kept ours for {} file(s), dropped {} file(s) upstream added", restored.len(), removed.len());

    auto_resolve(root)?;

    let report = review_report(root, &manifest, &base, &reference)?;
    let dir = root.join("target/upstream");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let report_path = dir.join(format!("review-{date}.md"));
    std::fs::write(&report_path, report.text).map_err(|e| format!("{}: {e}", report_path.display()))?;
    println!("review report: {} ({} upstream commit(s) touched owned paths)", report_path.display(), report.commits);

    crate::rename::run(root, &["--upstream", "--since", "HEAD"])?;
    if let Err(e) = crate::brand::check(root) {
        println!("brand check findings (fix with brand constants, see docs/upstream-merge.md step 4):\n{e}");
    }

    let left = conflicted(root)?;
    if !left.is_empty() {
        let list: String = left.iter().map(|f| format!("\n  {f}")).collect();
        return Err(format!("{} conflicted file(s) left; resolve them, `git add`, then `cargo xtask ci` and `git commit`:{list}", left.len()));
    }
    println!("merge staged on {branch} (not committed)");
    if run_ci {
        crate::cmd_ci()?;
    }
    println!("next: git commit (the merge), then commit name/brand fixes separately; review {}", report_path.display());
    Ok(())
}

/// Settles the conflict hunks that are not real conflicts once upstream's crate names are mapped to ours: ours equal
/// to the (renamed) base takes theirs, theirs equal to the base or to ours keeps ours. A file left without markers
/// is staged.
fn auto_resolve(root: &Path) -> Result<(), String> {
    let names = crate::rename::packages()?;
    let current = crate::rename::detect_prefix(&names).ok_or("cannot detect the current crate prefix")?;
    let suffixes: Vec<String> = names.iter().filter_map(|n| n.strip_prefix(&format!("{current}-")).map(str::to_string)).collect();
    let map = |t: &str| crate::rename::rewrite_upstream(t, &current, &suffixes);
    let (mut hunks, mut files) = (0, 0);
    for path in conflicted(root)? {
        let full = root.join(&path);
        let Ok(text) = std::fs::read_to_string(&full) else { continue };
        let (out, resolved, left) = resolve_hunks(&text, &map);
        if resolved == 0 {
            continue;
        }
        hunks += resolved;
        std::fs::write(&full, out).map_err(|e| format!("{}: {e}", full.display()))?;
        if left == 0 {
            git(root, &["add", "--", &path])?;
            files += 1;
        }
    }
    println!("auto-resolved {hunks} conflict hunk(s) that differed only by crate names; {files} file(s) fully resolved");
    Ok(())
}

/// Resolves the trivial zdiff3 hunks in `text`; returns the new text, the hunks resolved and the hunks left.
pub fn resolve_hunks(text: &str, map: &dyn Fn(&str) -> String) -> (String, usize, usize) {
    let mut out = String::with_capacity(text.len());
    let (mut resolved, mut left) = (0, 0);
    let mut rest = text;
    while let Some(start) = find_marker(rest, "<<<<<<<") {
        let hunk = rest.get(start..).unwrap_or_default();
        let parts = (|| {
            let ours_at = hunk.find('\n')? + 1;
            let base_m = find_marker(hunk, "|||||||")?;
            let base_at = base_m + hunk.get(base_m..)?.find('\n')? + 1;
            let sep = find_marker(hunk, "=======")?;
            let theirs_at = sep + hunk.get(sep..)?.find('\n')? + 1;
            let end_m = find_marker(hunk, ">>>>>>>")?;
            let end = hunk.get(end_m..)?.find('\n').map_or(hunk.len(), |n| end_m + n + 1);
            if !(ours_at <= base_m && base_m < base_at && base_at <= sep && sep < theirs_at && theirs_at <= end_m) {
                return None;
            }
            Some((hunk.get(ours_at..base_m)?, hunk.get(base_at..sep)?, hunk.get(theirs_at..end_m)?, end))
        })();
        let Some((ours, base, theirs, end)) = parts else { break };
        out.push_str(rest.get(..start).unwrap_or_default());
        if ours == base || ours == map(base) {
            out.push_str(theirs);
            resolved += 1;
        } else if theirs == base || ours == theirs || ours == map(theirs) {
            out.push_str(ours);
            resolved += 1;
        } else {
            out.push_str(hunk.get(..end).unwrap_or_default());
            left += 1;
        }
        rest = hunk.get(end..).unwrap_or_default();
    }
    out.push_str(rest);
    (out, resolved, left)
}

/// Byte offset of the first line that starts with `marker`.
fn find_marker(text: &str, marker: &str) -> Option<usize> {
    if text.starts_with(marker) {
        return Some(0);
    }
    text.find(&format!("\n{marker}")).map(|i| i + 1)
}

pub struct Report {
    pub text: String,
    pub commits: usize,
}

/// Upstream commits in `base..reference` that touched owned paths, with per-file line counts.
fn review_report(root: &Path, manifest: &Manifest, base: &str, reference: &str) -> Result<Report, String> {
    let range = format!("{base}..{reference}");
    let log = git(root, &["log", "--no-renames", "--numstat", "--format=%x01%H%x09%s", &range])?;
    Ok(build_report(&log, manifest, &range))
}

pub fn build_report(log: &str, manifest: &Manifest, range: &str) -> Report {
    let mut text = format!(
        "# Upstream changes to owned paths ({range})\n\nSet aside by `cargo xtask upstream-merge`: these changes were not merged. Review each for value (usually a bug fix) and port it by hand.\n\n"
    );
    let mut commits = 0;
    for chunk in log.split('\x01').filter(|c| !c.trim().is_empty()) {
        let mut it = chunk.lines();
        let header = it.next().unwrap_or("");
        let (hash, subject) = header.split_once('\t').unwrap_or((header, ""));
        let mut files = Vec::new();
        for l in it {
            let mut cols = l.splitn(3, '\t');
            let (Some(add), Some(del), Some(path)) = (cols.next(), cols.next(), cols.next()) else { continue };
            if manifest.is_owned(path) {
                files.push(format!("  - `{path}` +{add} -{del}"));
            }
        }
        if !files.is_empty() {
            commits += 1;
            let short = hash.get(..10).unwrap_or(hash);
            text.push_str(&format!("- [ ] `{short}` {subject}\n{}\n", files.join("\n")));
        }
    }
    if commits == 0 {
        text.push_str("Nothing: upstream did not touch owned paths.\n");
    }
    Report { text, commits }
}

// ---------------------------------------------------------------- upstream-pr

pub fn pr(root: &Path, args: &[&str]) -> Result<(), String> {
    let usage = "usage: cargo xtask upstream-pr <branch> <commit>...";
    let (branch, commits) = args.split_first().ok_or(usage)?;
    if commits.is_empty() || branch.starts_with('-') {
        return Err(usage.into());
    }
    require_clean(root)?;
    let names = crate::rename::packages()?;
    let current = crate::rename::detect_prefix(&names).ok_or("cannot detect the current crate prefix")?;
    // the app package is plain `UPSTREAM_PREFIX` upstream; handled as `-p` below
    let suffixes: Vec<String> =
        names.iter().filter_map(|n| n.strip_prefix(&format!("{current}-"))).filter(|s| *s != "app").map(str::to_string).collect();
    let to_upstream = |t: &str| {
        crate::rename::rewrite_known(t, &current, UPSTREAM_PREFIX, &suffixes)
            .replace(&format!("-p {current}-app "), &format!("-p {UPSTREAM_PREFIX} "))
    };
    // resolve commits before switching branches
    let mut shas = Vec::new();
    for c in commits {
        shas.push(git(root, &["rev-parse", "--verify", &format!("{c}^{{commit}}")])?.trim().to_string());
    }
    let _ = git(root, &["fetch", "upstream"]).map_err(|e| eprintln!("warning: {e}"));
    git(root, &["switch", "-c", branch, DEFAULT_REF])?;
    for sha in &shas {
        // map names in the patch itself, so it applies to upstream's spelling of the context lines
        let patch = git(root, &["format-patch", "-1", "--stdout", sha])?;
        let patch = to_upstream(&patch);
        let mut am =
            Command::new("git").current_dir(root).args(["am", "--3way"]).stdin(Stdio::piped()).spawn().map_err(|e| format!("git am: {e}"))?;
        if let Some(mut stdin) = am.stdin.take() {
            stdin.write_all(patch.as_bytes()).map_err(|e| format!("git am: {e}"))?;
        }
        let status = am.wait().map_err(|e| format!("git am: {e}"))?;
        if !status.success() {
            return Err(format!(
                "{sha} did not apply on {DEFAULT_REF}; resolve, `git am --continue` (or `git am --abort`), then re-run for the remaining commits"
            ));
        }
    }
    // anything the patches carried that the mapping missed (e.g. new files)
    let files = lines(&git(root, &["diff", "--name-only", DEFAULT_REF, "HEAD"])?);
    let changed = crate::rename::apply(root, &files, to_upstream)?;
    if !changed.is_empty() {
        git(root, &["commit", "-q", "-a", "-m", "Use upstream crate names"])?;
    }
    println!("branch {branch}: {} commit(s) on {DEFAULT_REF}, crate names mapped {current}-* -> {UPSTREAM_PREFIX}-*", shas.len());
    println!("check it builds on upstream (cargo test), then, yourself:");
    println!("  git push <your-fork-of-upstream> {branch}");
    println!("  open a PR against upstream main; put `UPSTREAM-PR: <link>` in our copy of the commit message");
    Ok(())
}

// ---------------------------------------------------------------- shared-check

/// `ci` passes `ci = true`: warn only, short list.
pub fn shared_check(root: &Path, args: &[&str], ci: bool) -> Result<(), String> {
    let strict = args.contains(&"--strict");
    let fail = |msg: String| {
        if strict {
            Err(msg)
        } else {
            eprintln!("shared-check: warning: {msg}");
            Ok(())
        }
    };
    let manifest = match Manifest::load(root) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let Some(base) = last_upstream_merge(root) else {
        return fail(format!("no `{DEFAULT_REF}` merge and no `{FORK_BASE}` tag here; skipped"));
    };
    let range = format!("{base}..HEAD");
    let log = match git(root, &["log", "--no-merges", "--no-renames", "--name-only", "--format=%x01%H%x02%s%x02%B%x03", &range]) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    let offenders = untagged_shared(&log, &manifest);
    if offenders.is_empty() {
        println!("shared-check: every change to shared paths since {} is tagged UPSTREAM-PR", short(&base));
        return Ok(());
    }
    let mut msg =
        format!("{} commit(s) since {} change shared paths without an `UPSTREAM-PR:` line (plan/upstream.md):", offenders.len(), short(&base));
    let limit = if ci { CI_LIST_LIMIT } else { usize::MAX };
    for (hash, subject, files) in offenders.iter().take(limit) {
        msg.push_str(&format!("\n  {} {subject} ({} shared file(s), e.g. {})", short(hash), files.len(), files.first().map_or("", String::as_str)));
    }
    if offenders.len() > limit {
        msg.push_str(&format!("\n  … {} more: cargo xtask shared-check", offenders.len() - limit));
    }
    fail(msg)
}

fn short(h: &str) -> &str {
    h.get(..10).unwrap_or(h)
}

/// The most recent merge on the first-parent line whose second parent upstream has; else the fork-base tag.
fn last_upstream_merge(root: &Path) -> Option<String> {
    if git_ok(root, &["rev-parse", "-q", "--verify", DEFAULT_REF]) {
        let merges = git(root, &["rev-list", "--merges", "--first-parent", "--max-count=500", "HEAD"]).ok()?;
        for m in merges.lines() {
            let Ok(p2) = git(root, &["rev-parse", "-q", "--verify", &format!("{m}^2")]) else { continue };
            if git_ok(root, &["merge-base", "--is-ancestor", p2.trim(), DEFAULT_REF]) {
                return Some(m.to_string());
            }
        }
    }
    git(root, &["rev-parse", "-q", "--verify", &format!("{FORK_BASE}^{{commit}}")]).ok().map(|s| s.trim().to_string())
}

/// From `git log --name-only --format=%x01%H%x02%s%x02%B%x03`: (hash, subject, shared files) of untagged commits.
pub fn untagged_shared(log: &str, manifest: &Manifest) -> Vec<(String, String, Vec<String>)> {
    let mut out = Vec::new();
    for chunk in log.split('\x01').filter(|c| !c.trim().is_empty()) {
        let (head, files) = chunk.split_once('\x03').unwrap_or((chunk, ""));
        let mut parts = head.splitn(3, '\x02');
        let (hash, subject, body) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if has_upstream_pr_tag(body) {
            continue;
        }
        let shared: Vec<String> = files.lines().map(str::trim).filter(|f| !f.is_empty() && manifest.is_shared(f)).map(str::to_string).collect();
        if !shared.is_empty() {
            out.push((hash.trim().to_string(), subject.to_string(), shared));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(s: &str) -> Pattern {
        Pattern::parse(s).unwrap()
    }

    #[test]
    fn globs() {
        assert!(pat("crates/catalog/").matches("crates/catalog/src/lib.rs"));
        assert!(!pat("crates/catalog/").matches("crates/catalog2/src/lib.rs"));
        assert!(!pat("crates/catalog/").matches("crates/catalog"));
        assert!(pat("brand.toml").matches("brand.toml"));
        assert!(!pat("brand.toml").matches("crates/brand.toml.bak"));
        assert!(pat("*.md").matches("docs/a/b.md"));
        assert!(pat("*.md").matches("README.md"));
        assert!(!pat("*.md").matches("x.md.in"));
        assert!(pat("PLAN*.md").matches("PLAN_phase_1.md"));
        assert!(!pat("/PLAN*.md").matches("docs/PLAN.md"));
        assert!(pat("**/locales/").matches("crates/ui-egui/locales/de/main.ftl"));
        assert!(pat("**/locales/").matches("locales/x.ftl"));
        assert!(pat("crates/**/lib.rs").matches("crates/raw/src/lib.rs"));
        assert!(pat("crates/**/lib.rs").matches("crates/lib.rs"));
        assert!(pat("a/?.rs").matches("a/b.rs"));
        assert!(!pat("a/?.rs").matches("a/bc.rs"));
        assert!(glob_segment("*a*b*", "xxaybz"));
        assert!(!glob_segment("*a*b", "xxaybz"));
        assert!(Pattern::parse("  ").is_none());
    }

    #[test]
    fn manifest_sections() {
        let m = Manifest::parse("# c\n[owned]\ncrates/catalog/  # trailing\nplan/\n[regenerated]\nCargo.lock\n[exempt]\n*.md\n").unwrap();
        assert!(m.is_owned("crates/catalog/Cargo.toml"));
        assert!(!m.is_shared("crates/catalog/Cargo.toml"));
        assert!(!m.is_shared("Cargo.lock"));
        assert!(!m.is_shared("docs/x.md"));
        assert!(m.is_shared("crates/raw/src/lib.rs"));
        assert!(Manifest::parse("x\n").is_err());
        assert!(Manifest::parse("[bogus]\n").is_err());
    }

    #[test]
    fn repo_manifest_parses() {
        let m = Manifest::load(&crate::root()).unwrap();
        assert!(m.is_owned("crates/catalog/src/lib.rs"));
        assert!(m.is_owned("crates/ui-egui/src/panels/classic.rs"));
        assert!(m.is_shared("crates/ui-egui/src/app.rs"));
        assert!(!m.is_shared("crates/ui-egui/locales/en/main.ftl"));
        assert!(!m.is_shared("docs/parity.md"));
    }

    #[test]
    fn upstream_pr_tag() {
        assert!(has_upstream_pr_tag("Fix crash\n\nUPSTREAM-PR: https://example.com/pr/1\n"));
        assert!(has_upstream_pr_tag("x\n  UPSTREAM-PR: #12"));
        assert!(!has_upstream_pr_tag("x\nUPSTREAM-PR:\n"));
        assert!(!has_upstream_pr_tag("mentions UPSTREAM-PR: inline"));
        assert!(!has_upstream_pr_tag("upstream-pr: lower"));
    }

    #[test]
    fn untagged_commits() {
        let m = Manifest::parse("[owned]\ncrates/catalog/\n[exempt]\n*.md\n").unwrap();
        let log = "\x01aaa\x02one\x02one\n\x03\ncrates/raw/src/lib.rs\nREADME.md\n\
                   \x01bbb\x02two\x02two\n\nUPSTREAM-PR: https://x/1\n\x03\ncrates/raw/src/lib.rs\n\
                   \x01ccc\x02three\x02three\n\x03\ncrates/catalog/src/lib.rs\ndocs/a.md\n";
        let got = untagged_shared(log, &m);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "aaa");
        assert_eq!(got[0].2, vec!["crates/raw/src/lib.rs".to_string()]);
    }

    #[test]
    fn report_lists_owned_only() {
        let m = Manifest::parse("[owned]\ncrates/catalog/\n").unwrap();
        let log = "\x010123456789abc\tFix catalog\n\n3\t1\tcrates/catalog/src/a.rs\n2\t2\tcrates/raw/x.rs\n\x01fff\tOther\n\n1\t0\tcrates/raw/y.rs\n";
        let r = build_report(log, &m, "a..b");
        assert_eq!(r.commits, 1);
        assert!(r.text.contains("`0123456789` Fix catalog"));
        assert!(r.text.contains("`crates/catalog/src/a.rs` +3 -1"));
        assert!(!r.text.contains("raw/x.rs"));
    }

    #[test]
    fn resolves_rename_only_hunks() {
        let map = |t: &str| t.replace("up_", "dac_");
        let text = "a\n<<<<<<< HEAD\nuse dac_x;\n||||||| base\nuse up_x;\n=======\nuse up_x::y;\n>>>>>>> upstream/main\nb\n<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> upstream/main\nc\n<<<<<<< HEAD\nmine\n||||||| base\nold\n=======\nold\n>>>>>>> upstream/main\n";
        let (out, resolved, left) = resolve_hunks(text, &map);
        assert_eq!((resolved, left), (2, 1));
        assert_eq!(out, "a\nuse up_x::y;\nb\n<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> upstream/main\nc\nmine\n");
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_736), (2026, 10, 10));
    }
}
