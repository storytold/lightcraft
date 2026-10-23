//! Embeds every `data/cameras/*.toml` (one file per maker) as `(file name, text)` pairs in `$OUT_DIR/cameras.rs`.
use std::fmt::Write as _;

fn main() {
    let dir = std::path::Path::new("data/cameras");
    println!("cargo:rerun-if-changed=data/cameras");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect())
        .unwrap_or_default();
    files.sort();
    let mut out = String::from("&[\n");
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let abs = std::fs::canonicalize(f).unwrap_or_else(|_| f.clone());
        let _ = writeln!(out, "    ({name:?}, include_str!({:?})),", abs.display().to_string());
    }
    out.push(']');
    let target = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("cameras.rs");
    if let Err(e) = std::fs::write(&target, out) {
        println!("cargo:warning=camdb: could not write {}: {e}", target.display());
    }
}
