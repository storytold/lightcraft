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
        // scan the head only: inner attributes live at the top of a crate root
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

/// Build the model from `cargo metadata --format-version 1 --no-deps` output. `src_path` for bin
/// targets is absolute in metadata output; libs use the manifest's `src/lib.rs`.
pub fn from_metadata(meta: &serde_json::Value) -> Result<Vec<CrateRoot>, String> {
    let pkgs = meta["packages"].as_array().ok_or("metadata: no packages array")?;
    let mut out = Vec::new();
    for p in pkgs {
        let name = p["name"].as_str().ok_or("package without name")?.to_owned();
        let manifest_dir = std::path::Path::new(p["manifest_path"].as_str().ok_or("package without manifest_path")?)
            .parent()
            .ok_or("manifest without parent")?
            .to_path_buf();
        for t in p["targets"].as_array().into_iter().flatten() {
            let mut kinds = t["kind"].as_array().into_iter().flatten();
            let is_lib = kinds.by_ref().any(|k| k.as_str() == Some("lib"));
            let is_bin = kinds.any(|k| k.as_str() == Some("bin"));
            let root = if is_lib {
                Some(manifest_dir.join("src/lib.rs"))
            } else if is_bin {
                Some(t["src_path"].as_str().map_or_else(|| manifest_dir.join("src/main.rs"), PathBuf::from))
            } else {
                None
            };
            if let Some(root) = root {
                let src = std::fs::read_to_string(&root).unwrap_or_default();
                out.push(CrateRoot { name: name.clone(), root, src });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
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
    fn deny_must_contain_all_six_lints() {
        let v = check(&[root("lightcraft-foo", "#![forbid(unsafe_code)]\n#![deny(clippy::unwrap_used, clippy::expect_used)]\nfn f() {}\n")]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }
}
