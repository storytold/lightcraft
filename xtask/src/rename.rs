//! `cargo xtask rename-crates`: the internal crate prefix is a codename that never reaches users, and this command
//! is the only place it changes.
//!
//! - `cargo xtask rename-crates <prefix>`: rewrite every `dac-*` / `dac_*` package and crate path in the workspace
//!   (manifests, `Cargo.lock`, code, docs, scripts) to `<prefix>-*` / `<prefix>_*`. The current prefix is detected
//!   from the workspace packages (override with `--from OLD`).
//! - `cargo xtask rename-crates --upstream [--since REV]`: during an upstream merge, rewrite the upstream crate
//!   paths `lightcraft-<crate>` / `lightcraft_<crate>` to the current prefix in every file that differs from REV
//!   (default `fork-base`) or has a merge conflict. Only names of crates this workspace has are rewritten, so
//!   binary names, file names and prose mentioning the upstream product are left for `cargo xtask brand check`.
//!   The upstream app package `lightcraft` (now `<prefix>-app`) is rewritten only as `-p lightcraft`.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// The upstream project's crate prefix.
pub const UPSTREAM_PREFIX: &str = "lightcraft";

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Every `old-x` / `old_x` that starts a token (not preceded by a word character or `-`) becomes `new-x` /
/// `new_x`; also the bare prefix strings `"old-"` / `"old_"`.
pub fn rewrite_all(text: &str, old: &str, new: &str) -> String {
    rewrite(text, old, new, |rest, _| rest.first().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'"'))
}

/// Only `old-<suffix>` / `old_<suffix_with_underscores>` for the given crate suffixes (longest first), each ending
/// at a token boundary.
pub fn rewrite_known(text: &str, old: &str, new: &str, suffixes: &[String]) -> String {
    let mut sorted: Vec<&String> = suffixes.iter().collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
    rewrite(text, old, new, |rest, sep| {
        sorted.iter().any(|s| {
            let s = if sep == b'_' { s.replace('-', "_") } else { s.to_string() };
            rest.starts_with(s.as_bytes()) && rest.get(s.len()).is_none_or(|c| !is_word(*c) && *c != b'-')
        })
    })
}

fn rewrite(text: &str, old: &str, new: &str, accept: impl Fn(&[u8], u8) -> bool) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(i) = text[from..].find(old) {
        let at = from + i;
        let end = at + old.len();
        from = end;
        let boundary = at == 0 || !(is_word(bytes[at - 1]) || bytes[at - 1] == b'-');
        let Some(&sep) = bytes.get(end) else { break };
        if !boundary || (sep != b'-' && sep != b'_') {
            continue;
        }
        // `"old-"`: the bare prefix as a string literal
        let bare = at > 0 && bytes[at - 1] == b'"' && bytes.get(end + 1) == Some(&b'"');
        if bare || accept(&bytes[end + 1..], sep) {
            out.push_str(&text[last..at]);
            out.push_str(new);
            last = end;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Workspace package names (from `cargo metadata`).
fn packages() -> Result<Vec<String>, String> {
    let meta = crate::metadata()?;
    let pkgs = meta["packages"].as_array().ok_or("cargo metadata: no packages")?;
    Ok(pkgs.iter().filter_map(|p| p["name"].as_str().map(str::to_string)).collect())
}

/// The prefix most workspace packages share (`dac` for `dac-raw`, `dac-engine`, …).
pub fn detect_prefix(names: &[String]) -> Option<String> {
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for n in names {
        if let Some((p, _)) = n.split_once('-') {
            *counts.entry(p).or_default() += 1;
        }
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(p, _)| p.to_string())
}

fn valid_prefix(p: &str) -> bool {
    !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) && p.as_bytes()[0].is_ascii_lowercase()
}

fn git_lines(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let out = Command::new("git").current_dir(root).args(args).output().map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
}

/// Rewrites `files` with `f`; returns how many changed. Binary, missing and allowlisted files are skipped.
fn apply(root: &Path, files: &[String], f: impl Fn(&str) -> String) -> Result<Vec<String>, String> {
    let mut changed = Vec::new();
    for rel in files {
        if crate::brand::is_allowed(rel) {
            continue;
        }
        let path = root.join(rel);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if bytes.contains(&0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else { continue };
        let new = f(&text);
        if new != text {
            std::fs::write(&path, new).map_err(|e| format!("{}: {e}", path.display()))?;
            changed.push(rel.clone());
        }
    }
    Ok(changed)
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let usage = "usage: cargo xtask rename-crates <prefix> [--from OLD] | --upstream [--since REV]";
    let mut upstream = false;
    let mut since = "fork-base".to_string();
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--upstream" => upstream = true,
            "--since" => {
                i += 1;
                since = args.get(i).ok_or(usage)?.to_string();
            }
            "--from" => {
                i += 1;
                from = Some(args.get(i).ok_or(usage)?.to_string());
            }
            a if a.starts_with('-') => return Err(format!("unknown option `{a}`\n{usage}")),
            a => to = Some(a.to_string()),
        }
        i += 1;
    }
    let names = packages()?;
    let current = detect_prefix(&names).ok_or("cannot detect the current crate prefix")?;

    if upstream {
        let suffixes: Vec<String> = names.iter().filter_map(|n| n.strip_prefix(&format!("{current}-")).map(str::to_string)).collect();
        let mut files: BTreeSet<String> = git_lines(root, &["diff", "--name-only", &since])?.into_iter().collect();
        files.extend(git_lines(root, &["diff", "--name-only", "--diff-filter=U"])?);
        let files: Vec<String> = files.into_iter().collect();
        let app_old = format!("-p {UPSTREAM_PREFIX} ");
        let app_new = format!("-p {current}-app ");
        let changed = apply(root, &files, |t| rewrite_known(t, UPSTREAM_PREFIX, &current, &suffixes).replace(&app_old, &app_new))?;
        println!(
            "rename-crates --upstream: {UPSTREAM_PREFIX}-* -> {current}-* in {} of {} file(s) changed since {since}",
            changed.len(),
            files.len()
        );
        for f in &changed {
            println!("  {f}");
        }
        println!("next: cargo xtask brand check (remaining upstream product names), cargo xtask ci");
        return Ok(());
    }

    let to = to.ok_or(usage)?;
    let from = from.unwrap_or(current);
    if !valid_prefix(&to) || !valid_prefix(&from) {
        return Err(format!("prefixes are lower-case ASCII letters and digits, starting with a letter: `{from}` -> `{to}`"));
    }
    if from == to {
        return Err(format!("the crate prefix is already `{to}`"));
    }
    let files = crate::brand::source_files(root)?;
    let changed = apply(root, &files, |t| rewrite_all(t, &from, &to))?;
    println!("rename-crates: {from}-* -> {to}-* in {} file(s)", changed.len());
    for f in &changed {
        println!("  {f}");
    }
    println!("next: cargo check --workspace && cargo xtask ci, then commit the rename on its own");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_all_tokens() {
        let src = "dac-raw = { path = \"crates/raw\" }\nuse dac_engine::Session;\npkg.strip_prefix(\"dac-\")\nadac-x dac x-dac-y\n";
        let out = rewrite_all(src, "dac", "foo");
        assert_eq!(out, "foo-raw = { path = \"crates/raw\" }\nuse foo_engine::Session;\npkg.strip_prefix(\"foo-\")\nadac-x dac x-dac-y\n");
    }

    #[test]
    fn upstream_rewrites_known_crates_only() {
        let suffixes = vec!["denoise".to_string(), "denoise-core".to_string(), "ui-egui".to_string(), "cli".to_string()];
        let src = "lightcraft-denoise-core lightcraft_ui_egui::x lightcraft-cli lightcraft_zh_hans.otf lightcraft-0.2.tar.gz LightCraft";
        let out = rewrite_known(src, "lightcraft", "dac", &suffixes);
        assert_eq!(out, "dac-denoise-core dac_ui_egui::x dac-cli lightcraft_zh_hans.otf lightcraft-0.2.tar.gz LightCraft");
        assert_eq!(rewrite_known("lightcraft-denoise-extra", "lightcraft", "dac", &suffixes), "lightcraft-denoise-extra");
    }

    #[test]
    fn detects_prefix() {
        let names: Vec<String> = ["dac-raw", "dac-app", "xtask", "dac-cli", "other-thing"].iter().map(|s| s.to_string()).collect();
        assert_eq!(detect_prefix(&names).as_deref(), Some("dac"));
        assert!(valid_prefix("abc1") && !valid_prefix("1abc") && !valid_prefix("a-b"));
    }
}
