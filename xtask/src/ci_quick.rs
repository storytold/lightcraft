//! `cargo xtask ci --quick`: the checks a branch's own changes can break, for a pre-push check.
//!
//! The full `ci` builds the workspace four ways (clippy, heif, tests, wasm) plus the CLI for the
//! parity check, ~23 minutes cold. `--quick` looks at what changed since the merge base with
//! `origin/main` (or `--base REF`), committed or not, and runs:
//!
//! - fmt, layers, assets: always (seconds);
//! - clippy on the whole workspace (check builds only; catches callers broken by an API change);
//! - the tests of the changed packages only (not their dependents);
//! - heif when `lightcraft-codecs` / `lightcraft-heif` changed;
//! - parity when the tracker docs, a command/control crate, or a file's path changed (renames and
//!   deletions: the tracker cites paths);
//! - the wasm check of the changed packages that must build for wasm32.
//!
//! A file outside any package that a crate compiles in (`include_str!` of `docs/whats-new.md`,
//! fonts and camera profiles under `assets/`, ...) counts as a change to that crate (`EMBEDDED`).
//! A change to a workspace-wide file (root `Cargo.toml`, `Cargo.lock`, lint or toolchain config)
//! tests everything. The full `ci` (GitHub's CI on every PR) stays the merge gate.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Root-level files whose change can affect every package.
const WORKSPACE_WIDE: &[&str] =
    &["Cargo.toml", "Cargo.lock", "clippy.toml", "rustfmt.toml", ".rustfmt.toml", "rust-toolchain", "rust-toolchain.toml"];
const WORKSPACE_WIDE_DIRS: &[&str] = &[".cargo/"];
/// Packages whose ids or paths the parity tracker cites (commands, develop controls, UI menus).
const PARITY_PACKAGES: &[&str] = &["lightcraft-engine", "lightcraft-develop", "lightcraft-ui-egui", "lightcraft-cli"];
const PARITY_DOCS: &[&str] = &["docs/parity-checklist.md", "docs/gaps.md"];
const HEIF_PACKAGES: &[&str] = &["lightcraft-codecs", "lightcraft-heif"];
/// Files outside a package's directory that it compiles in (`include_str!` / `include_bytes!`,
/// a build script's `rerun-if-changed`): a change to one counts as a change to those packages.
/// An entry ending in `/` covers the directory. `embedded_files_are_mapped` keeps this complete.
const EMBEDDED: &[(&str, &[&str])] = &[
    ("assets/app-icon/", &["lightcraft"]),
    ("assets/camera-profiles/", &["lightcraft-engine"]),
    ("assets/camera-tone/", &["lightcraft-engine"]),
    ("assets/fonts/", &["lightcraft-engine", "lightcraft-ui-egui"]),
    ("contributors/", &["lightcraft-ui-egui"]),
    ("docs/whats-new.md", &["lightcraft-ui-egui"]),
    ("docs/raw/coverage-sweep.tsv", &["lightcraft-raw"]),
    ("docs/raf-corpus.sha256", &["lightcraft-raw"]),
    ("xtask/src/raw_samples.rs", &["lightcraft-raw"]),
    ("crates/ui-egui/src/menus.rs", &["xtask"]),
    ("crates/ui-egui/src/menus/", &["xtask"]),
];

/// The packages that compile in `path` from outside their own directory.
fn embedders(path: &str) -> impl Iterator<Item = &'static str> + '_ {
    EMBEDDED.iter().filter(move |(p, _)| if p.ends_with('/') { path.starts_with(p) } else { path == *p }).flat_map(|(_, pkgs)| pkgs.iter().copied())
}

/// A changed path, relative to the workspace root; `gone` when deleted or renamed away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    pub gone: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Workspace packages with a changed file.
    pub packages: BTreeSet<String>,
    /// A workspace-wide file changed: test everything.
    pub workspace_wide: bool,
    pub heif: bool,
    pub parity: bool,
    /// What the changes were measured against, for the log and the report.
    pub base: String,
    pub files: usize,
}

impl Plan {
    /// Whether anything compiled changed (otherwise clippy and the tests are skipped).
    pub fn rust(&self) -> bool {
        self.workspace_wide || !self.packages.is_empty()
    }
}

/// Decide what to run. `packages` are (name, directory relative to the root, `/`-separated).
pub fn plan(changed: &[Changed], packages: &[(String, String)], base: String) -> Plan {
    let mut p = Plan { base, files: changed.len(), ..Plan::default() };
    for c in changed {
        let path = c.path.as_str();
        if WORKSPACE_WIDE.contains(&path) || WORKSPACE_WIDE_DIRS.iter().any(|d| path.starts_with(d)) {
            p.workspace_wide = true;
        }
        if PARITY_DOCS.contains(&path) || c.gone {
            p.parity = true;
        }
        // the innermost package directory containing the file
        let owner = packages
            .iter()
            .filter(|(_, dir)| !dir.is_empty() && path.strip_prefix(dir.as_str()).is_some_and(|rest| rest.starts_with('/')))
            .max_by_key(|(_, dir)| dir.len());
        if let Some((name, _)) = owner {
            p.packages.insert(name.clone());
        }
        for name in embedders(path) {
            if packages.iter().any(|(n, _)| n == name) {
                p.packages.insert(name.to_owned());
            }
        }
    }
    p.heif = p.workspace_wide || HEIF_PACKAGES.iter().any(|h| p.packages.contains(*h));
    p.parity |= p.workspace_wide || PARITY_PACKAGES.iter().any(|h| p.packages.contains(*h));
    p
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").args(args).current_dir(root).output().map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `git diff --name-status -z` output: NUL-separated fields, a status then its path, or
/// two paths (old, new) for a rename or copy. `-z` leaves non-ASCII paths unquoted.
fn parse_name_status(out: &str) -> Vec<Changed> {
    let mut v = Vec::new();
    let mut fields = out.split('\0');
    while let Some(status) = fields.next() {
        let Some(kind) = status.trim().chars().next() else { continue };
        let Some(first) = fields.next() else { break };
        match kind {
            // renamed / copied: old path, new path
            'R' | 'C' => {
                v.push(Changed { path: first.to_owned(), gone: kind == 'R' });
                if let Some(second) = fields.next() {
                    v.push(Changed { path: second.to_owned(), gone: false });
                }
            }
            'D' => v.push(Changed { path: first.to_owned(), gone: true }),
            _ => v.push(Changed { path: first.to_owned(), gone: false }),
        }
    }
    v
}

/// Parse NUL-separated `git ls-files -z` output.
fn parse_paths_z(out: &str) -> impl Iterator<Item = Changed> + '_ {
    out.split('\0').filter(|p| !p.is_empty()).map(|p| Changed { path: p.to_owned(), gone: false })
}

/// The files changed since the merge base with `base` (default `origin/main`, else `main`):
/// committed, staged, unstaged and untracked. Returns the base description and the files.
pub fn changed_files(root: &Path, base: Option<&str>) -> Result<(String, Vec<Changed>), String> {
    let base = match base {
        Some(b) => b.to_owned(),
        None => ["origin/main", "main"]
            .into_iter()
            .find(|b| git(root, &["rev-parse", "--verify", "--quiet", &format!("{b}^{{commit}}")]).is_ok())
            .ok_or("no origin/main or main to compare with; pass --base REF")?
            .to_owned(),
    };
    let merge_base = git(root, &["merge-base", "HEAD", &base])?.trim().to_owned();
    let mut files = parse_name_status(&git(root, &["diff", "--name-status", "-z", "-M", &merge_base])?);
    files.extend(parse_paths_z(&git(root, &["ls-files", "-z", "--others", "--exclude-standard"])?));
    let short: String = merge_base.chars().take(10).collect();
    Ok((format!("{base} (merge base {short})"), files))
}

/// (name, root-relative directory) of each workspace package, from `cargo metadata`.
pub fn packages(root: &Path, metadata: &serde_json::Value) -> Vec<(String, String)> {
    let roots: Vec<std::path::PathBuf> = [Some(root.to_path_buf()), root.canonicalize().ok()].into_iter().flatten().collect();
    metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let name = p["name"].as_str()?;
            let dir = Path::new(p["manifest_path"].as_str()?).parent()?;
            let rel = roots.iter().find_map(|r| dir.strip_prefix(r).ok())?;
            let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            Some((name.to_owned(), rel))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(path: &str) -> Changed {
        Changed { path: path.into(), gone: false }
    }

    fn pkgs() -> Vec<(String, String)> {
        [
            ("lightcraft-raw", "crates/raw"),
            ("lightcraft-codecs", "crates/codecs"),
            ("lightcraft-engine", "crates/engine"),
            ("lightcraft-cli", "apps/lightcraft-cli"),
            ("xtask", "xtask"),
            ("lightcraft-raw-nested", "crates/raw/nested"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect()
    }

    #[test]
    fn changed_files_pick_their_innermost_package() {
        let p = plan(&[ch("crates/raw/src/lib.rs"), ch("crates/raw/nested/src/x.rs"), ch("crates/rawx/y.rs")], &pkgs(), "b".into());
        assert_eq!(p.packages.iter().map(String::as_str).collect::<Vec<_>>(), ["lightcraft-raw", "lightcraft-raw-nested"]);
        assert!(p.rust() && !p.workspace_wide && !p.heif && !p.parity);
    }

    #[test]
    fn docs_only_skip_the_builds_but_tracker_docs_run_parity() {
        let p = plan(&[ch("README.md"), ch("docs/raw-parity.md")], &pkgs(), "b".into());
        assert!(!p.rust() && !p.parity);
        let p = plan(&[ch("docs/gaps.md")], &pkgs(), "b".into());
        assert!(!p.rust() && p.parity);
    }

    #[test]
    fn workspace_files_heif_and_parity_triggers() {
        let p = plan(&[ch("Cargo.lock")], &pkgs(), "b".into());
        assert!(p.workspace_wide && p.rust() && p.heif && p.parity);
        let p = plan(&[ch("crates/codecs/src/heif.rs")], &pkgs(), "b".into());
        assert!(p.heif && !p.parity);
        let p = plan(&[ch("crates/engine/src/commands.rs")], &pkgs(), "b".into());
        assert!(p.parity && !p.heif);
        let p = plan(&[Changed { path: "crates/raw/src/old.rs".into(), gone: true }], &pkgs(), "b".into());
        assert!(p.parity, "a removed path may be cited by the tracker");
    }

    #[test]
    fn name_status_parses_renames_and_deletions() {
        let v = parse_name_status("M\0a.rs\0R087\0old.rs\0new.rs\0D\0gone.rs\0C100\0src.rs\0copy.rs\0A\0added.rs\0");
        assert_eq!(
            v,
            [
                ch("a.rs"),
                Changed { path: "old.rs".into(), gone: true },
                ch("new.rs"),
                Changed { path: "gone.rs".into(), gone: true },
                ch("src.rs"),
                ch("copy.rs"),
                ch("added.rs"),
            ]
        );
        // truncated output: no panic, keeps what it could read
        assert_eq!(parse_name_status("M\0a.rs\0R100\0x.rs"), [ch("a.rs"), Changed { path: "x.rs".into(), gone: true }]);
    }

    #[test]
    fn non_ascii_paths_still_map_to_their_package() {
        // without -z, git prints these quoted and octal-escaped ("crates/raw/src/\303\251t\303\251.rs")
        let v = parse_name_status("M\0crates/raw/src/été.rs\0R100\0crates/engine/a.rs\0crates/engine/日本.rs\0");
        assert_eq!(v[0], ch("crates/raw/src/été.rs"));
        assert_eq!(v[2], ch("crates/engine/日本.rs"));
        let untracked: Vec<_> = parse_paths_z("crates/codecs/ünï code.rs\0").collect();
        assert_eq!(untracked, [ch("crates/codecs/ünï code.rs")]);
        let p = plan(&[v, untracked].concat(), &pkgs(), "b".into());
        assert_eq!(p.packages.iter().map(String::as_str).collect::<Vec<_>>(), ["lightcraft-codecs", "lightcraft-engine", "lightcraft-raw"]);
    }

    #[test]
    fn embedded_files_outside_a_crate_test_the_crates_that_embed_them() {
        let p = plan(&[ch("docs/raw/coverage-sweep.tsv")], &pkgs(), "b".into());
        assert!(p.rust());
        assert_eq!(p.packages.iter().map(String::as_str).collect::<Vec<_>>(), ["lightcraft-raw"]);
        let p = plan(&[ch("assets/fonts/Inter-Regular.ttf")], &pkgs(), "b".into());
        assert!(p.rust() && p.packages.contains("lightcraft-engine"));
        // a consumer missing from the workspace's package list is not invented
        assert!(!p.packages.contains("lightcraft-ui-egui"));
        let p = plan(&[ch("assets/ATTRIBUTION.md"), ch("docs/raw-parity.md")], &pkgs(), "b".into());
        assert!(!p.rust(), "only files something compiles in");
    }

    /// Every `include_str!` / `include_bytes!` of a file outside its package, and every `"../…"`
    /// path in a build script, is listed in `EMBEDDED` for that package.
    #[test]
    fn embedded_files_are_mapped() {
        use std::path::{Component, PathBuf};
        fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() && p.file_name().is_some_and(|n| n != "target") {
                    rs_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask is in the workspace").to_path_buf();
        // `a/b/../c` -> `a/c`, relative to the root; None outside it
        let normalize = |p: &Path| -> Option<String> {
            let mut parts: Vec<String> = Vec::new();
            for c in p.strip_prefix(&root).ok()?.components() {
                match c {
                    Component::ParentDir => {
                        parts.pop()?;
                    }
                    Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
                    _ => {}
                }
            }
            Some(parts.join("/"))
        };
        let mut files = Vec::new();
        for d in ["crates", "apps", "xtask"] {
            rs_files(&root.join(d), &mut files);
        }
        let (mut checked, mut missing) = (0, Vec::new());
        for file in files {
            let Ok(src) = std::fs::read_to_string(&file) else { continue };
            // the package: the nearest directory with a Cargo.toml
            let Some(pkg_dir) = file.ancestors().skip(1).find(|d| d.join("Cargo.toml").is_file()) else { continue };
            let manifest = std::fs::read_to_string(pkg_dir.join("Cargo.toml")).unwrap_or_default();
            let Some(name) = manifest.lines().find_map(|l| l.strip_prefix("name = \"")?.strip_suffix('"')) else { continue };
            let Some(pkg_rel) = normalize(pkg_dir) else { continue };
            let mut targets = Vec::new();
            for mac in ["include_str!(", "include_bytes!("] {
                for (i, _) in src.match_indices(mac) {
                    let rest = src[i + mac.len()..].trim_start();
                    if let Some(lit) = rest.strip_prefix('"').and_then(|r| r.split('"').next()) {
                        targets.push(file.parent().expect("a file has a directory").join(lit));
                    }
                }
            }
            if file.file_name().is_some_and(|n| n == "build.rs") {
                for (i, _) in src.match_indices("\"../") {
                    if let Some(lit) = src[i + 1..].split('"').next() {
                        targets.push(pkg_dir.join(lit));
                    }
                }
            }
            for t in targets {
                // outside the workspace (`../craft-fonts`), not a real file, or inside the package
                let Some(rel) = normalize(&t) else { continue };
                if !root.join(&rel).exists() || rel.starts_with(&format!("{pkg_rel}/")) {
                    continue;
                }
                checked += 1;
                // a directory (`rerun-if-changed=../../contributors`) needs a `dir/` entry
                let rel = if root.join(&rel).is_dir() { format!("{rel}/") } else { rel };
                if !embedders(&rel).any(|p| p == name) {
                    missing.push(format!("{rel} (compiled into {name} by {})", file.display()));
                }
            }
        }
        assert!(checked > 0, "found no embedded files: the scan is broken");
        assert!(missing.is_empty(), "add these to ci_quick::EMBEDDED:\n{}", missing.join("\n"));
    }
}
