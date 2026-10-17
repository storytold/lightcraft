//! `cargo xtask notes`: release notes kept one file per change (`lcc-notes`, `docs/whats-new/README.md`).
//!
//! - `--check` (a `cargo xtask ci` step): every What's New entry (`docs/whats-new/`) and changelog entry
//!   (`docs/changelog/unreleased/`) is well-formed, and `docs/whats-new.md`, upstream's entries only, is the file
//!   the last upstream merge recorded (`docs/whats-new/upstream.sha256`). An entry added there instead of in its
//!   own file fails here, so the conflicts the files avoid don't come back.
//! - `--upstream`: records `docs/whats-new.md` as upstream's, after an upstream merge (`upstream-merge` does it).
//! - `--release <version> [--dry-run]`: gathers the changelog entries into `docs/changelog/<version>.md` (adding to
//!   a file already started by hand) and deletes them; `--dry-run` prints the result and changes nothing.

use std::path::Path;

use lcc_notes::{Kind, UPSTREAM_STAMP};

const WHATS_NEW: &str = "docs/whats-new.md";
const WHATS_NEW_DIR: &str = "docs/whats-new";
const CHANGELOG_DIR: &str = "docs/changelog";
const UNRELEASED: &str = "docs/changelog/unreleased";

const USAGE: &str = "usage: cargo xtask notes --check | --upstream | --release <version> [--dry-run]";

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    match args {
        ["--check"] => check(root),
        ["--upstream"] => record_upstream(root),
        ["--release", version] => release(root, version, false),
        ["--release", version, "--dry-run"] | ["--dry-run", "--release", version] => release(root, version, true),
        _ => Err(USAGE.to_string()),
    }
}

/// The errors, one per line, with paths relative to the repo and `/`-separated on every OS.
fn list(errors: &[lcc_notes::Error], root: &Path) -> String {
    let prefix = format!("{}/", root.display()).replace('\\', "/");
    errors.iter().map(|e| format!("\n  {}", e.to_string().replace('\\', "/").replace(&prefix, ""))).collect()
}

/// The hash of upstream's What's New, line endings aside (a Windows checkout may write CRLF).
fn whats_new_hash(root: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let path = root.join(WHATS_NEW);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(text.replace("\r\n", "\n").as_bytes())))
}

fn stamp_path(root: &Path) -> std::path::PathBuf {
    root.join(WHATS_NEW_DIR).join(UPSTREAM_STAMP)
}

/// Records `docs/whats-new.md` as upstream's, as the merge just left it.
pub fn record_upstream(root: &Path) -> Result<(), String> {
    let hash = whats_new_hash(root)?;
    let path = stamp_path(root);
    std::fs::write(&path, format!("{hash}  {WHATS_NEW}\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("notes: recorded {WHATS_NEW} as upstream's ({}/{UPSTREAM_STAMP})", WHATS_NEW_DIR);
    Ok(())
}

pub fn check(root: &Path) -> Result<(), String> {
    let mut problems = String::new();
    let mut counts = Vec::new();
    for (kind, dir, what) in [(Kind::WhatsNew, WHATS_NEW_DIR, "What's New"), (Kind::Changelog, UNRELEASED, "changelog")] {
        match lcc_notes::read_entries(kind, &root.join(dir)) {
            Ok(entries) => counts.push(format!("{what}: {}", count(entries.len()))),
            Err(errors) => problems.push_str(&format!("\n{what} entries to fix ({dir}/README.md):{}", list(&errors, root))),
        }
    }
    let recorded = std::fs::read_to_string(stamp_path(root)).unwrap_or_default();
    let recorded = recorded.split_whitespace().next().unwrap_or_default();
    match whats_new_hash(root) {
        Err(e) => problems.push_str(&format!("\n{e}")),
        Ok(hash) if hash != recorded => problems.push_str(&format!(
            "\n{WHATS_NEW} changed since the last upstream merge recorded it. It holds upstream's entries only: put a \
             new entry in its own file, {WHATS_NEW_DIR}/<YYYY-MM-DD>-<slug>.md ({WHATS_NEW_DIR}/README.md). After an \
             upstream merge, record the merged file with `cargo xtask notes --upstream`."
        )),
        Ok(_) => {}
    }
    if !problems.is_empty() {
        return Err(format!("notes:{problems}"));
    }
    println!("notes: well-formed: {}; {WHATS_NEW} is upstream's as last recorded", counts.join(", "));
    Ok(())
}

fn count(n: usize) -> String {
    format!("{n} {}", if n == 1 { "entry" } else { "entries" })
}

fn valid_version(version: &str) -> bool {
    version.starts_with(|c: char| c.is_ascii_digit()) && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

fn release(root: &Path, version: &str, dry_run: bool) -> Result<(), String> {
    if !valid_version(version) {
        return Err(format!("`{version}` is not a version (like 0.9.1 or 0.10.0-rc1); nothing changed\n{USAGE}"));
    }
    let dir = root.join(UNRELEASED);
    let entries = lcc_notes::read_entries(Kind::Changelog, &dir)
        .map_err(|errors| format!("changelog entries to fix first; nothing changed:{}", list(&errors, root)))?;
    if entries.is_empty() {
        return Err(format!("no changelog entries in {UNRELEASED}/; nothing changed"));
    }
    let target = root.join(CHANGELOG_DIR).join(format!("{version}.md"));
    let existing = match std::fs::read_to_string(&target) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}; nothing changed", target.display())),
    };
    let text = lcc_notes::release_changelog(existing.as_deref(), version, &entries);
    let shown = format!("{CHANGELOG_DIR}/{version}.md");
    if dry_run {
        print!("{text}");
        println!("\nnotes: --dry-run: {} would go into {shown}; nothing changed", count(entries.len()));
        return Ok(());
    }
    std::fs::write(&target, text).map_err(|e| format!("{}: {e}; nothing changed", target.display()))?;
    let mut left = Vec::new();
    for entry in &entries {
        let path = dir.join(&entry.name);
        if let Err(e) = std::fs::remove_file(&path) {
            left.push(format!("{}: {e}", path.display()));
        }
    }
    if !left.is_empty() {
        return Err(format!(
            "{shown} written, but these entries are still there; delete them by hand before running --release again \
             (it would add them twice):\n  {}",
            left.join("\n  ")
        ));
    }
    println!("notes: {} gathered into {shown} {}", count(entries.len()), if existing.is_some() { "(added to it)" } else { "(new)" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Root(PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn root(name: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("xtask-notes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in [WHATS_NEW_DIR, UNRELEASED] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join(WHATS_NEW), "# What's new in {app}\n\n## 2026-10-14\n\n### Up\n- u\n").unwrap();
        std::fs::write(dir.join(WHATS_NEW_DIR).join("2026-10-16-ours.md"), "### Ours\n- o\n").unwrap();
        record_upstream(&dir).unwrap();
        Root(dir)
    }

    fn write(root: &Path, rel: &str, text: &str) {
        std::fs::write(root.join(rel), text).unwrap();
    }

    #[test]
    fn check_passes_on_well_formed_entries_and_the_recorded_upstream_file() {
        let r = root("ok");
        write(&r.0, "docs/changelog/unreleased/2026-10-16-a.md", "## Fixes\n- **A**\n");
        assert_eq!(check(&r.0), Ok(()));
    }

    #[test]
    fn an_entry_added_to_upstreams_file_is_sent_to_its_own_file() {
        let r = root("edited");
        let path = r.0.join(WHATS_NEW);
        let text = std::fs::read_to_string(&path).unwrap().replace("## 2026-10-14", "## 2026-10-16\n\n### Mine\n- m\n\n## 2026-10-14");
        std::fs::write(&path, text).unwrap();
        let err = check(&r.0).unwrap_err();
        assert!(err.contains("docs/whats-new/<YYYY-MM-DD>-<slug>.md"), "the message says where entries go: {err}");
        record_upstream(&r.0).unwrap();
        assert_eq!(check(&r.0), Ok(()), "recording an upstream merge's result clears it");
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap().replace('\n', "\r\n")).unwrap();
        assert_eq!(check(&r.0), Ok(()), "a CRLF checkout is the same file");
    }

    #[test]
    fn check_names_each_malformed_entry() {
        let r = root("bad");
        write(&r.0, "docs/whats-new/2026-10-16-bad.md", "- no heading\n");
        write(&r.0, "docs/changelog/unreleased/2026-02-30-date.md", "## Fixes\n- a\n");
        let err = check(&r.0).unwrap_err();
        assert!(err.contains("docs/whats-new/2026-10-16-bad.md: line 1"), "{err}");
        assert!(err.contains("docs/changelog/unreleased/2026-02-30-date.md: `2026-02-30` is not a date"), "{err}");
        std::fs::remove_file(r.0.join(stamp_path(&r.0))).unwrap();
        assert!(check(&r.0).unwrap_err().contains("changed since the last upstream merge"), "no record is a mismatch");
    }

    #[test]
    fn release_gathers_the_entries_and_deletes_them() {
        let r = root("release");
        write(&r.0, "docs/changelog/unreleased/2026-10-16-a.md", "## Fixes\n- **A**\n");
        write(&r.0, "docs/changelog/unreleased/2026-10-17-b.md", "## Library\n- **B**\n");
        write(&r.0, "docs/changelog/unreleased/README.md", "How entries work\n");
        run(&r.0, &["--release", "0.9.1", "--dry-run"]).unwrap();
        assert!(!r.0.join("docs/changelog/0.9.1.md").exists(), "--dry-run writes nothing");
        assert!(r.0.join("docs/changelog/unreleased/2026-10-16-a.md").exists(), "--dry-run deletes nothing");
        run(&r.0, &["--release", "0.9.1"]).unwrap();
        let text = std::fs::read_to_string(r.0.join("docs/changelog/0.9.1.md")).unwrap();
        assert_eq!(text, "# 0.9.1\n\n## Library\n\n- **B**\n\n## Fixes\n\n- **A**\n");
        let left: Vec<String> =
            std::fs::read_dir(r.0.join(UNRELEASED)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["README.md"], "the gathered entries are gone, the README stays");
        let err = run(&r.0, &["--release", "0.9.1"]).unwrap_err();
        assert!(err.contains("no changelog entries"), "{err}");
    }

    #[test]
    fn release_changes_nothing_when_an_entry_is_malformed_or_the_version_is_odd() {
        let r = root("refuse");
        write(&r.0, "docs/changelog/unreleased/2026-10-16-a.md", "## Fixes\n- **A**\n");
        write(&r.0, "docs/changelog/unreleased/2026-10-16-b.md", "- no section\n");
        assert!(run(&r.0, &["--release", "0.9.1"]).unwrap_err().contains("nothing changed"));
        for version in ["../0.9.1", "v0.9.1", "", "0.9 1"] {
            assert!(run(&r.0, &["--release", version]).unwrap_err().contains("is not a version"), "{version:?}");
        }
        assert!(!r.0.join("docs/changelog/0.9.1.md").exists());
        assert!(r.0.join("docs/changelog/unreleased/2026-10-16-a.md").exists());
        assert!(run(&r.0, &["--release"]).is_err() && run(&r.0, &[]).is_err(), "usage errors");
    }
}
