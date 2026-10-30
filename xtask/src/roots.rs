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
    MissingForbid {
        name: String,
        path: PathBuf,
    },
    MissingDeny {
        name: String,
        path: PathBuf,
    },
    /// A crate-root `allow`/`expect`/`warn` names a required lint: rustc applies the later
    /// attribute over the deny list, so the lint is not denied after all.
    OverriddenDeny {
        name: String,
        path: PathBuf,
        attribute: String,
        lint: String,
    },
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
            Violation::OverriddenDeny { name, path, attribute, lint } => {
                write!(f, "{name}: the crate-root `#![{attribute}({lint})]` overrides the required deny list in {}", path.display())
            }
        }
    }
}

/// Check every crate root carries both attributes; violations sorted for stable output.
pub fn check(roots: &[CrateRoot]) -> Vec<Violation> {
    let mut out = Vec::new();
    for r in roots {
        let is_unsafe_exception = UNSAFE_EXCEPTION.contains(&r.name.as_str());
        // the inner attributes before the first item, however long the doc header above them is
        let attrs = inner_attrs(&r.src);
        let has_forbid = attrs.iter().any(|a| a == "#![forbid(unsafe_code)]");
        let has_deny =
            attrs.iter().any(|a| lint_list(a).is_some_and(|(verb, names)| verb == "deny" && DENY_LINTS.iter().all(|lint| names.contains(lint))));
        // a later crate-root `allow`/`expect`/`warn` naming a required lint applies over the deny
        // list (rustc takes the later attribute), so the policy silently stops holding
        let mut overridden: Option<(&str, &str)> = None;
        for a in &attrs {
            let Some((verb, names)) = lint_list(a) else { continue };
            if !matches!(verb, "allow" | "expect" | "warn") {
                continue;
            }
            if let Some(lint) = names.iter().find(|n| DENY_LINTS.contains(n)) {
                overridden = Some((verb, lint));
                break;
            }
        }
        if !has_forbid && !is_unsafe_exception {
            out.push(Violation::MissingForbid { name: r.name.clone(), path: r.root.clone() });
        }
        if !has_deny {
            out.push(Violation::MissingDeny { name: r.name.clone(), path: r.root.clone() });
        }
        if let Some((attribute, lint)) = overridden {
            out.push(Violation::OverriddenDeny {
                name: r.name.clone(),
                path: r.root.clone(),
                attribute: attribute.to_string(),
                lint: lint.to_string(),
            });
        }
    }
    out.sort_by_key(|v| v.to_string());
    out.dedup();
    out
}

/// The verb and the lint names of a root `#![verb(...)]` attribute, or `None` for anything else.
/// Names are compared whole, so a longer name that starts with a listed name does not count.
fn lint_list(attr: &str) -> Option<(&str, Vec<&str>)> {
    let body = attr.strip_prefix("#![")?.strip_suffix(")]")?;
    let (verb, names) = body.split_once('(')?;
    Some((verb, names.split(',').map(str::trim).filter(|n| !n.is_empty()).collect()))
}

/// The inner attributes (`#![...]`) at the top of a crate root, each with its whitespace removed.
/// Blank lines and comments before them are skipped, and the scan stops at the first item, so the
/// length of the doc header does not matter and an attribute inside a nested module is never read.
fn inner_attrs(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = skip_trivia(b, 0);
    while b.get(i..).is_some_and(|rest| rest.starts_with(b"#![")) {
        let start = i;
        let mut depth = 0usize;
        let mut in_str = false;
        while let Some(&c) = b.get(i) {
            i += 1;
            if in_str {
                match c {
                    b'\\' => i += 1,
                    b'"' => in_str = false,
                    _ => {}
                }
            } else {
                match c {
                    b'"' => in_str = true,
                    b'[' => depth += 1,
                    b']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        let Some(text) = src.get(start..i) else { break };
        out.push(text.split_whitespace().collect());
        i = skip_trivia(b, i);
    }
    out
}

/// The first index at or after `i` that is neither whitespace nor part of a comment.
fn skip_trivia(b: &[u8], mut i: usize) -> usize {
    loop {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let rest = b.get(i..).unwrap_or_default();
        if rest.starts_with(b"//") {
            while b.get(i).is_some_and(|&c| c != b'\n') {
                i += 1;
            }
        } else if rest.starts_with(b"/*") {
            i = skip_block_comment(b, i);
        } else {
            return i;
        }
    }
}

/// The index just past the block comment that starts at `i` (block comments nest, as in Rust).
fn skip_block_comment(b: &[u8], mut i: usize) -> usize {
    let mut depth = 0usize;
    loop {
        match b.get(i..) {
            Some(rest) if rest.starts_with(b"/*") => {
                depth += 1;
                i += 2;
            }
            Some(rest) if rest.starts_with(b"*/") => {
                depth = depth.saturating_sub(1);
                i += 2;
                if depth == 0 {
                    return i;
                }
            }
            Some(_) => i += 1,
            None => return i,
        }
    }
}

/// Collect the (package name, root source path) pairs for every workspace lib/bin target.
/// `src_path` from the package metadata is absolute and is used as it is; without it, libs default to the
/// manifest's `src/lib.rs` and binaries to `src/main.rs`.
/// Library kinds include `rlib`/`cdylib` so roots like the web build's `crate-type =
/// ["cdylib", "rlib"]` are covered, and `proc-macro`, which cargo reports as a kind of its own.
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
            let is_lib = kinds.iter().any(|&k| matches!(k, "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"));
            let is_bin = kinds.contains(&"bin");
            if is_lib || is_bin {
                let fallback = if is_lib { "src/lib.rs" } else { "src/main.rs" };
                let root = match t["src_path"].as_str() {
                    Some(s) => PathBuf::from(s),
                    None => manifest_dir.join(fallback),
                };
                out.push((name.clone(), root));
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
    fn root_paths_collects_proc_macro_targets() {
        let meta = serde_json::json!({
            "packages": [
                { "name": "lightcraft-macros", "manifest_path": "/ws/macros/Cargo.toml",
                  "targets": [ { "kind": ["proc-macro"], "src_path": "/ws/macros/src/derive.rs" } ] },
                { "name": "lightcraft-plain-macros", "manifest_path": "/ws/plain-macros/Cargo.toml",
                  "targets": [ { "kind": ["proc-macro"] } ] },
            ]
        });
        let paths = root_paths(&meta).expect("metadata should parse");
        assert_eq!(
            paths,
            vec![
                ("lightcraft-macros".to_owned(), PathBuf::from("/ws/macros/src/derive.rs")),
                ("lightcraft-plain-macros".to_owned(), PathBuf::from("/ws/plain-macros/src/lib.rs")),
            ]
        );
    }

    #[test]
    fn deny_must_contain_all_six_lints() {
        let v = check(&[root("lightcraft-foo", "#![forbid(unsafe_code)]\n#![deny(clippy::unwrap_used, clippy::expect_used)]\nfn f() {}\n")]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }

    #[test]
    fn a_root_allow_overriding_a_required_lint_is_reported() {
        // rustc applies the later attribute, so the deny list above it no longer denies
        let src = format!("#![forbid(unsafe_code)]\n#![deny({DENY})]\n#![allow(clippy::panic)]\nfn f() {{}}\n");
        let v = check(&[root("lightcraft-foo", &src)]);
        assert!(matches!(&v[..], [Violation::OverriddenDeny { lint, .. }] if lint == "clippy::panic"), "{v:?}");
        assert_eq!(
            v.first().map(|v| v.to_string()).as_deref(),
            Some("lightcraft-foo: the crate-root `#![allow(clippy::panic)]` overrides the required deny list in src/lightcraft-foo.rs"),
            "the rendered message shows the attribute, not literal braces",
        );
    }

    #[test]
    fn a_root_expect_overriding_a_required_lint_is_reported() {
        let src = format!("#![forbid(unsafe_code)]\n#![deny({DENY})]\n#![expect(clippy::unwrap_used)]\nfn f() {{}}\n");
        let v = check(&[root("lightcraft-foo", &src)]);
        assert!(matches!(&v[..], [Violation::OverriddenDeny { lint, .. }] if lint == "clippy::unwrap_used"), "{v:?}");
    }

    #[test]
    fn an_item_level_allow_inside_a_function_is_not_an_override() {
        // the documented guarded-expect form: the crate-level deny list stays intact
        let src = format!(
            "#![forbid(unsafe_code)]\n#![deny({DENY})]\nfn f() {{\n    #[allow(clippy::expect_used)] // invariant\n    let x: u8 = None.expect(\"checked above\");\n}}\n"
        );
        assert!(check(&[root("lightcraft-foo", &src)]).is_empty());
    }

    #[test]
    fn library_root_follows_the_declared_lib_path() {
        let meta = serde_json::json!({
            "packages": [
                { "name": "lightcraft-custom", "manifest_path": "/ws/custom/Cargo.toml",
                  "targets": [ { "kind": ["lib"], "src_path": "/ws/custom/src/core.rs" } ] },
            ]
        });
        let paths = root_paths(&meta).expect("metadata should parse");
        assert_eq!(paths, vec![("lightcraft-custom".to_owned(), PathBuf::from("/ws/custom/src/core.rs"))]);
    }

    #[test]
    fn a_lint_named_only_in_a_trailing_comment_does_not_count() {
        let src = "#![forbid(unsafe_code)]\n#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo)] // clippy::unreachable\nfn f() {}\n";
        let v = check(&[root("lightcraft-foo", src)]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }

    #[test]
    fn lint_names_must_match_whole() {
        let src = "#![forbid(unsafe_code)]\n#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic_in_result_fn, clippy::unimplemented, clippy::todo, clippy::unreachable)]\nfn f() {}\n";
        let v = check(&[root("lightcraft-foo", src)]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }

    #[test]
    fn attributes_after_a_long_header_are_found() {
        let header: String = (0..40).map(|n| format!("//! header line {n}\n")).collect();
        let src = format!("{header}\n#![forbid(unsafe_code)]\n#![deny({DENY})]\nfn f() {{}}\n");
        assert!(check(&[root("lightcraft-foo", &src)]).is_empty());
    }

    #[test]
    fn multi_line_deny_attribute_is_read() {
        let src = "#![forbid(unsafe_code)]\n#![deny(\n    clippy::unwrap_used,\n    clippy::expect_used,\n    clippy::panic,\n    clippy::unimplemented,\n    clippy::todo,\n    clippy::unreachable,\n)]\nfn f() {}\n";
        assert!(check(&[root("lightcraft-foo", src)]).is_empty());
    }

    #[test]
    fn attributes_of_a_nested_module_do_not_count() {
        let src = format!("#![forbid(unsafe_code)]\nfn f() {{}}\nmod inner {{\n    #![deny({DENY})]\n}}\n");
        let v = check(&[root("lightcraft-foo", &src)]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }

    #[test]
    fn a_commented_out_attribute_does_not_count() {
        let src = format!("#![forbid(unsafe_code)]\n/*\n#![deny({DENY})]\n*/\nfn f() {{}}\n");
        let v = check(&[root("lightcraft-foo", &src)]);
        assert!(matches!(&v[..], [Violation::MissingDeny { .. }]), "{v:?}");
    }

    #[test]
    fn targets_without_src_path_fall_back_to_the_default_files() {
        let meta = serde_json::json!({
            "packages": [
                { "name": "lightcraft-plain", "manifest_path": "/ws/plain/Cargo.toml",
                  "targets": [ { "kind": ["lib"] } ] },
                { "name": "lightcraft-tool", "manifest_path": "/ws/tool/Cargo.toml",
                  "targets": [ { "kind": ["bin"] } ] },
            ]
        });
        let paths = root_paths(&meta).expect("metadata should parse");
        assert_eq!(paths[0].1, PathBuf::from("/ws/plain/src/lib.rs"));
        assert_eq!(paths[1].1, PathBuf::from("/ws/tool/src/main.rs"));
    }
}
