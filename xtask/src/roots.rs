//! Crate-root lint attributes: every production crate root carries both `#![forbid(unsafe_code)]`
//! and the deny list (`clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`,
//! `clippy::unimplemented`, `clippy::todo`, `clippy::unreachable`).
//!
//! Machine-enforced so a future crate can't silently drop the never-crash policy (AGENTS.md).
//! `check` works on already-read file contents so it is unit-testable without the real tree.
//!
//! Exception (documented in AGENTS.md): `crates/sysmem` holds LightCraft's only allowed `unsafe`
//! (one FFI call), so it is exempt from `forbid(unsafe_code)` — it still needs the deny list, and
//! carries it.

use std::path::PathBuf;

const DENY_LINTS: &[&str] =
    &["clippy::unwrap_used", "clippy::expect_used", "clippy::panic", "clippy::unimplemented", "clippy::todo", "clippy::unreachable"];

/// Crate roots exempt from `forbid(unsafe_code)` (they still need the deny list).
const UNSAFE_EXCEPTION: &[&str] = &["lightcraft-sysmem"];

#[derive(Debug, Clone)]
pub struct CrateRoot {
    pub name: String,
    /// The root source file (`src/lib.rs` for libs, `src/main.rs` for binaries).
    pub root: PathBuf,
    /// Its contents, already read (kept separate so `check` needs no filesystem).
    pub src: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    MissingForbid { name: String, path: PathBuf },
    MissingDeny { name: String, path: PathBuf },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::MissingForbid { name, path } => {
                write!(f, "{name}: missing `#![forbid(unsafe_code)]` in {}", path.display())
            }
            Violation::MissingDeny { name, path } => {
                write!(f, "{name}: missing `#![deny(...)]` with all six clippy lints in {}", path.display())
            }
        }
    }
}

/// Check every crate root carries both attributes; violations sorted for stable output.
pub fn check(roots: &[CrateRoot]) -> Vec<Violation> {
    let mut out = Vec::new();
    for r in roots {
        let is_unsafe_exception = UNSAFE_EXCEPTION.contains(&r.name.as_str());
        // scan only the head: inner attributes precede the first item (30 = longest current doc
        // header, segment at 25, plus margin)
        let head: Vec<&str> = r.src.lines().take(30).collect();
        let has_forbid = head.iter().any(|l| l.trim() == "#![forbid(unsafe_code)]");
        let has_deny = head.iter().any(|l| {
            let t = l.trim();
            t.starts_with("#![deny(") && DENY_LINTS.iter().all(|lint| t.contains(lint))
        });
        if !has_forbid && !is_unsafe_exception {
            out.push(Violation::MissingForbid { name: r.name.clone(), path: r.root.clone() });
        }
        if !has_deny {
            out.push(Violation::MissingDeny { name: r.name.clone(), path: r.root.clone() });
        }
    }
    out.sort_by_key(|v| v.to_string());
    out.dedup();
    out
}

/// Collect the (package name, root source path) pairs for every workspace lib/bin target.
/// `src_path` from cargo metadata is absolute; libs default to the manifest's `src/lib.rs`.
/// Library kinds include `rlib`/`cdylib` so roots like the web build's `crate-type =
/// ["cdylib", "rlib"]` are covered.
pub fn root_paths(meta: &serde_json::Value) -> Result<Vec<(String, PathBuf)>, String> {
    let pkgs = meta["packages"].as_array().ok_or("metadata: no packages array")?;
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for p in pkgs {
        let name = p["name"].as_str().ok_or("package without name")?.to_owned();
        let manifest_dir = std::path::Path::new(p["manifest_path"].as_str().ok_or("package without manifest_path")?)
            .parent()
            .ok_or("manifest without parent")?
            .to_path_buf();
        for t in p["targets"].as_array().into_iter().flatten() {
            let kinds: Vec<&str> = t["kind"].as_array().into_iter().flatten().filter_map(|k| k.as_str()).collect();
            let is_lib = kinds.iter().any(|&k| matches!(k, "lib" | "rlib" | "dylib" | "cdylib" | "staticlib"));
            let is_bin = kinds.contains(&"bin");
            if is_lib {
                out.push((name.clone(), manifest_dir.join("src/lib.rs")));
            }
            if is_bin {
                match t["src_path"].as_str() {
                    Some(s) => out.push((name.clone(), PathBuf::from(s))),
                    None => out.push((name.clone(), manifest_dir.join("src/main.rs"))),
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Read each root's contents (separate from [`root_paths`] so the path-collection logic is
/// unit-testable without the real tree).
pub fn from_metadata(meta: &serde_json::Value) -> Result<Vec<CrateRoot>, String> {
    let mut out = Vec::new();
    for (name, root) in root_paths(meta)? {
        let src = std::fs::read_to_string(&root).map_err(|e| format!("roots: {}: {e}", root.display()))?;
        out.push(CrateRoot { name, root, src });
    }
    Ok(out)
}

/// Run the roots check against the real workspace tree.
pub fn run() -> Result<(), String> {
    let roots = from_metadata(&super::metadata()?)?;
    let violations = check(&roots);
    if violations.is_empty() {
        println!("OK: {} crate roots carry both `#![forbid(unsafe_code)]` and the deny list.", roots.len());
        Ok(())
    } else {
        Err(violations.iter().map(|v| format!("  {v}")).collect::<Vec<_>>().join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str, src: &str) -> CrateRoot {
        CrateRoot { name: name.into(), root: PathBuf::from(format!("src/{name}.rs")), src: src.into() }
    }

    const DENY: &str = "clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable";

    #[test]
    fn both_attributes_pass() {
        let src = format!("#![forbid(unsafe_code)]\n#![deny({DENY})]\nfn f() {{}}\n");
        assert!(check(&[root("lightcraft-foo", &src)]).is_empty());
    }

    #[test]
    fn missing_forbid_reported() {
        let src = format!("#![deny({DENY})]\nfn f() {{}}\n");
        let v = check(&[root("lightcraft-foo", &src)]);
        assert!(matches!(&v[..], [Violation::MissingForbid { .. }]), "{v:?}");
    }

    #[test]
    fn missing_deny_reported() {
        let v = check(&[root("lightcraft-foo", "#![forbid(unsafe_code)]\nfn f() {}\n")]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }

    #[test]
    fn sysmem_exempt_from_forbid_but_not_deny() {
        let good = format!("#![deny({DENY})]\nfn f() {{}}\n");
        assert!(check(&[root("lightcraft-sysmem", &good)]).is_empty());
        let v = check(&[root("lightcraft-sysmem", "#![forbid(unsafe_code)]\nfn f() {}\n")]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }

    #[test]
    fn root_paths_collects_bin_cdylib_and_lib_targets() {
        let meta = serde_json::json!({
            "packages": [
                { "name": "lightcraft-bin-only", "manifest_path": "/ws/bin-only/Cargo.toml",
                  "targets": [ { "kind": ["bin"], "src_path": "/ws/bin-only/src/main.rs" } ] },
                { "name": "lightcraft-meta", "manifest_path": "/ws/meta/Cargo.toml",
                  "targets": [ { "kind": ["lib"], "src_path": "/ws/meta/src/lib.rs" } ] },
                { "name": "lightcraft-web", "manifest_path": "/ws/web/Cargo.toml",
                  "targets": [ { "kind": ["cdylib", "rlib"], "src_path": "/ws/web/src/lib.rs" } ] },
            ]
        });
        let paths = root_paths(&meta).expect("metadata should parse");
        let names: Vec<_> = paths.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["lightcraft-bin-only", "lightcraft-meta", "lightcraft-web"]);
        assert_eq!(paths[0].1, std::path::Path::new("/ws/bin-only/src/main.rs"));
        assert_eq!(paths[1].1, std::path::Path::new("/ws/meta/src/lib.rs"));
        assert_eq!(paths[2].1, std::path::Path::new("/ws/web/src/lib.rs"));
    }

    #[test]
    fn deny_must_contain_all_six_lints() {
        let v = check(&[root("lightcraft-foo", "#![forbid(unsafe_code)]\n#![deny(clippy::unwrap_used, clippy::expect_used)]\nfn f() {}\n")]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }
}
