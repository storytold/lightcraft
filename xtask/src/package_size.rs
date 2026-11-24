//! `cargo xtask package-size [--update] [--budget FILE] FILE…`: the release package-size check
//! (#1232). Each release download is compared with its stored budget in
//! `packaging/size-budget.toml` (bytes per download, keyed by its name without the program name
//! and version: `<binary>-26.48.0-linux-x86_64.AppImage` → `linux-x86_64.AppImage`). A download
//! more than [`GROWTH_LIMIT_PERCENT`] over its budget fails the check, and so does one without a
//! budget (a pre-release, `26.48.0-rc.1`, uses the release's). `--update` rewrites the budget with the given files' sizes (a reviewed
//! change: commit it with the reason the package grew).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How far over its budget a download may grow before the check fails.
pub const GROWTH_LIMIT_PERCENT: u64 = 10;

const DEFAULT_BUDGET: &str = "packaging/size-budget.toml";

/// `<binary>-<version>-<rest>` → `<rest>`; `<binary>-web-<version>.zip` → `web.zip`. None for a
/// name with no `N.N.N` version in it, and for a side file published next to a download (the
/// AppImage's `.zsync` delta-update index), which is not a download of its own.
pub fn budget_key(file_name: &str) -> Option<String> {
    if file_name.ends_with(".zsync") {
        return None;
    }
    let parts: Vec<&str> = file_name.split('-').collect();
    let at = parts.iter().position(|part| strip_version(part).is_some())?;
    // the version may carry the extension (`26.48.0.zip`): keep what follows its three numbers
    let suffix = strip_version(parts.get(at)?)?;
    let mut key: Vec<&str> = parts.get(1..at).unwrap_or_default().to_vec();
    let mut rest: Vec<&str> = parts.get(at + 1..).unwrap_or_default().to_vec();
    let mut suffix = suffix.to_string();
    // a pre-release (`26.48.0-rc.1-linux-…`, `web-26.48.0-rc.1.zip`) shares the release's budget
    if suffix.is_empty()
        && let Some(first) = rest.first()
        && let Some(ext) = strip_prerelease(first)
    {
        if ext.is_empty() {
            rest.remove(0);
        } else if rest.len() == 1 {
            suffix = ext.to_string();
            rest.clear();
        }
    }
    key.extend(rest);
    let key = format!("{}{suffix}", key.join("-"));
    (!key.is_empty()).then_some(key)
}

/// `26.48.0` → `""`, `26.48.0.zip` → `".zip"`; None unless it starts with three dotted numbers.
fn strip_version(part: &str) -> Option<&str> {
    let mut rest = part;
    for i in 0..3 {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = rest.get(digits..)?;
        if i < 2 {
            rest = rest.strip_prefix('.')?;
        }
    }
    (rest.is_empty() || rest.starts_with('.')).then_some(rest)
}

/// `rc.1` → `""`, `rc.1.zip` → `".zip"`, `beta` → `""`: a pre-release tag (letters, then
/// dot-separated numbers) and what follows it. None for anything else (`linux`, `x86_64.AppImage`).
fn strip_prerelease(part: &str) -> Option<&str> {
    let word = part.bytes().take_while(u8::is_ascii_alphabetic).count();
    let tag = part.get(..word)?.to_ascii_lowercase();
    if !["alpha", "beta", "rc", "pre", "preview", "dev", "nightly"].contains(&tag.as_str()) {
        return None;
    }
    let mut rest = part.get(word..)?;
    while let Some(after) = rest.strip_prefix('.') {
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            break;
        }
        rest = after.get(digits..)?;
    }
    (rest.is_empty() || rest.starts_with('.')).then_some(rest)
}

/// The verdict for one download: `Err` when it grew past the limit, or has no budget (a new or
/// renamed download must get one, or it would never be checked).
pub fn judge(size: u64, budget: Option<u64>) -> Result<String, String> {
    let Some(budget) = budget else {
        return Err(format!("{}: no budget entry; add one with `cargo xtask package-size --update FILE` (and remove the old name's)", mb(size)));
    };
    let limit = budget.saturating_add(budget / 100 * GROWTH_LIMIT_PERCENT + budget % 100 * GROWTH_LIMIT_PERCENT / 100);
    let change = if budget == 0 { 0.0 } else { (size as f64 / budget as f64 - 1.0) * 100.0 };
    let line = format!("{} vs budget {} ({change:+.1} %)", mb(size), mb(budget));
    if size > limit { Err(format!("{line}: more than {GROWTH_LIMIT_PERCENT} % over budget")) } else { Ok(line) }
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

fn read_budget(path: &Path) -> Result<BTreeMap<String, u64>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn write_budget(path: &Path, budget: &BTreeMap<String, u64>) -> Result<(), String> {
    let mut text = String::from(
        "# Release download sizes in bytes (`cargo xtask package-size`, docs/release-process.md).\n\
         # The release check fails when a download grows more than 10 % over its entry.\n\
         # Raise an entry with `cargo xtask package-size --update FILE…` and say why in the commit.\n",
    );
    for (key, size) in budget {
        text.push_str(&format!("{key:?} = {size}\n"));
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let mut update = false;
    let mut budget_path = root.join(DEFAULT_BUDGET);
    let mut files: Vec<PathBuf> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "--update" => update = true,
            "--budget" => budget_path = PathBuf::from(it.next().ok_or("--budget needs a file")?),
            flag if flag.starts_with("--") => return Err(format!("package-size: unknown option {flag}")),
            file => files.push(PathBuf::from(file)),
        }
    }
    if files.is_empty() {
        return Err("usage: cargo xtask package-size [--update] [--budget FILE] FILE…".into());
    }
    let mut budget = read_budget(&budget_path)?;
    let mut failures = Vec::new();
    for file in &files {
        let name = file.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("{}: no file name", file.display()))?;
        let Some(key) = budget_key(name) else {
            println!("{name}: skipped (not a versioned download)");
            continue;
        };
        let size = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.len();
        if update {
            println!("{key}: budget set to {}", mb(size));
            budget.insert(key, size);
            continue;
        }
        match judge(size, budget.get(&key).copied()) {
            Ok(line) => println!("{key}: {line}"),
            Err(line) => {
                println!("{key}: {line}");
                failures.push(key);
            }
        }
    }
    if update {
        return write_budget(&budget_path, &budget);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "package size check failed for {} (raise the budget with `cargo xtask package-size --update` only with a reason)",
            failures.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_drop_the_program_name_and_version() {
        assert_eq!(budget_key("app-26.48.0-linux-x86_64.AppImage").as_deref(), Some("linux-x86_64.AppImage"));
        assert_eq!(budget_key("app-26.48.0-windows-x64-portable.zip").as_deref(), Some("windows-x64-portable.zip"));
        assert_eq!(budget_key("app-web-26.48.0.zip").as_deref(), Some("web.zip"));
        assert_eq!(budget_key("app-26.48.10-macos-arm64.dmg").as_deref(), Some("macos-arm64.dmg"));
        // pre-releases share the release's entry
        assert_eq!(budget_key("app-26.48.0-rc.1-linux-x86_64.AppImage").as_deref(), Some("linux-x86_64.AppImage"));
        assert_eq!(budget_key("app-26.48.0-beta-macos-arm64.dmg").as_deref(), Some("macos-arm64.dmg"));
        assert_eq!(budget_key("app-web-26.48.0-rc.2.zip").as_deref(), Some("web.zip"));
        assert_eq!(budget_key("app-26.48.0-linux-x86_64.rpm").as_deref(), Some("linux-x86_64.rpm"));
        assert_eq!(budget_key("app-26.48.0-linux-x86_64.AppImage.zsync"), None, "the Linux job's side file is skipped, not failed");
        assert_eq!(budget_key("SHA256SUMS.txt"), None);
        assert_eq!(budget_key("latest.json"), None);
        // hostile names never panic
        for name in ["", "-", "--", "a-1.2", "a-1..2.3-x", "a-.1.2.3", "a-1.2.3", "1.2.3", "a-99999999999999999999.1.1-é"] {
            let _ = budget_key(name);
        }
    }

    #[test]
    fn growth_past_the_limit_fails() {
        assert!(judge(110, Some(100)).is_ok());
        assert!(judge(111, Some(100)).is_err());
        assert!(judge(50, Some(100)).is_ok());
        assert!(judge(5, None).is_err(), "a download with no budget is not silently passed");
        assert!(judge(1, Some(0)).is_err());
        assert!(judge(u64::MAX, Some(u64::MAX)).is_ok());
    }

    #[test]
    fn the_stored_budget_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let budget = read_budget(&root.join(DEFAULT_BUDGET)).unwrap();
        assert!(!budget.is_empty());
        assert!(budget.values().all(|size| *size > 0));
    }
}
