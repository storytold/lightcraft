//! `cargo xtask docs [--check]`: renders `README.md.in` and every `*.md.in` below `docs/` into the `.md` next to it,
//! replacing `{{app}}` (the display name), `{{binary}}`, `{{cli_binary}}`, `{{env_prefix}}` and every other
//! `brand.toml` key. `--check` fails when a rendered file is missing or out of date (part of `cargo xtask ci`).

use std::path::{Path, PathBuf};

use crate::brand::{Brand, render};

/// The doc templates, sorted.
pub fn templates(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for p in entries.flatten().map(|e| e.path()) {
            if p.is_dir() {
                walk(&p, out);
            } else if p.to_string_lossy().ends_with(".md.in") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    let readme = root.join("README.md.in");
    if readme.is_file() {
        out.push(readme);
    }
    walk(&root.join("docs"), &mut out);
    out.sort();
    out
}

pub fn run(root: &Path, check: bool) -> Result<(), String> {
    let vars = Brand::load(&root.join("brand.toml"))?.vars();
    let mut stale = Vec::new();
    let list = templates(root);
    for src in &list {
        let text = std::fs::read_to_string(src).map_err(|e| format!("{}: {e}", src.display()))?;
        let rendered = render(&text, &vars).map_err(|e| format!("{}: {e}", src.display()))?;
        let dest = PathBuf::from(src.to_string_lossy().strip_suffix(".in").unwrap_or_default().to_string());
        let rel = dest.strip_prefix(root).unwrap_or(&dest).display().to_string();
        let current = std::fs::read_to_string(&dest).ok();
        if current.as_deref() == Some(rendered.as_str()) {
            continue;
        }
        if check {
            stale.push(rel);
        } else {
            std::fs::write(&dest, rendered).map_err(|e| format!("{}: {e}", dest.display()))?;
            println!("rendered {rel}");
        }
    }
    if !stale.is_empty() {
        return Err(format!("docs out of date (run `cargo xtask docs`): {}", stale.join(", ")));
    }
    println!("docs: {} template(s) {}", list.len(), if check { "up to date" } else { "rendered" });
    Ok(())
}
