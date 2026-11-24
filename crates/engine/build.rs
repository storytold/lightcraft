//! Optional build input: with `CRAFT_FONTS_DIR=<storytold/craft-fonts checkout>`, embed every font
//! in its `fonts/manifest.txt` as `fonts::craft_fonts` (see src/fonts.rs). Unset, it is empty.
//! Recipe from craft-fonts' docs/integration.md; craft-fonts is never a Cargo dependency, and this
//! script only reads a local directory (no network).
//!
//! wasm32 embeds BIZ UDPGothic Regular (~4.5 MB), Noto Sans Thai (~214 KiB), Noto Sans Georgian
//! and Noto Sans Hebrew Regular (~65 KiB, for Hebrew file and folder names), to keep the `.wasm`
//! within the web size budget (Cloudflare serves at most 25 MiB per file); the export watermark
//! falls back to it there. Native builds embed every font.
//!
//! `CRAFT_FONTS_EMBED=0` (native only) checks the manifest as usual but embeds nothing: the
//! package puts the manifest and its fonts beside the program instead (the macOS app bundle's
//! `Contents/Resources/fonts/`), where `fonts::craft_fonts` reads them at run time, so the app and
//! the CLI inside it share one copy.
//!
//! `CRAFT_FONTS_LANGUAGE=<code>` (opt-in, never for releases) builds for one UI language: only the
//! faces that language's UI needs are embedded (`src/ui_languages.rs`, none for Latin, Cyrillic and
//! Greek languages), and the UI offers only that language and English. An unknown code, or the
//! setting with `CRAFT_FONTS_REQUIRED` or `CRAFT_FONTS_EMBED=0`, fails the build.
#[path = "src/ui_languages.rs"]
#[allow(dead_code)]
mod ui_languages;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_DIR");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_REQUIRED");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_EMBED");
    println!("cargo::rerun-if-changed=src/ui_languages.rs");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_LANGUAGE");
    let required = std::env::var_os("CRAFT_FONTS_REQUIRED").is_some();
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "wasm32");
    // the web build has no files beside it: it always embeds
    let beside = !wasm && std::env::var_os("CRAFT_FONTS_EMBED").is_some_and(|v| v == "0");
    let language = std::env::var("CRAFT_FONTS_LANGUAGE").ok().map(|code| code.trim().to_ascii_lowercase()).filter(|code| !code.is_empty());
    let needed: Option<&'static [&'static str]> = match language.as_deref() {
        None => None,
        Some(code) => match ui_languages::scripts_for(code) {
            Some(scripts) => Some(scripts),
            None => {
                println!("cargo::error=CRAFT_FONTS_LANGUAGE={code} is not a UI language; valid codes: {}", ui_languages::valid_codes());
                Some(&[])
            }
        },
    };
    if language.is_some() && required {
        println!("cargo::error=CRAFT_FONTS_LANGUAGE and CRAFT_FONTS_REQUIRED conflict: release builds embed every language's fonts");
    }
    if language.is_some() && beside {
        println!("cargo::error=CRAFT_FONTS_LANGUAGE and CRAFT_FONTS_EMBED=0 conflict: a single-language build embeds its fonts");
    }
    let mut src = String::from("static EMBEDDED: &[CraftFont] = &[\n");
    let mut embedded = 0;
    if let Some(dir) = std::env::var_os("CRAFT_FONTS_DIR").map(PathBuf::from).map(resolve) {
        match craft_fonts(&dir, required, needed) {
            Ok(entries) => {
                // the checkout, for the tests that load it the way a package's copy is loaded
                if let Ok(root) = dir.canonicalize()
                    && !root.display().to_string().contains(['\n', '\r'])
                {
                    println!("cargo::rustc-env=CRAFT_FONTS_CHECKOUT={}", root.display());
                }
                embedded = entries.lines().count();
                if !beside {
                    src.push_str(&entries);
                }
            }
            Err(e) if required => {
                println!("cargo::error=CRAFT_FONTS_DIR={}: {e}", dir.display());
            }
            Err(e) => println!("cargo::warning=building without craft-fonts: CRAFT_FONTS_DIR={}: {e}", dir.display()),
        }
    } else if required {
        println!("cargo::error=CRAFT_FONTS_REQUIRED is set but CRAFT_FONTS_DIR is missing");
    }
    src.push_str("];\n");
    let _ = writeln!(
        src,
        "/// The one UI language this build was made for (`CRAFT_FONTS_LANGUAGE`), or `None` for every language.\npub const BUILD_LANGUAGE: Option<&str> = {:?};",
        language.filter(|code| ui_languages::scripts_for(code).is_some()),
    );
    if !wasm {
        let _ = writeln!(src, "/// Built with `CRAFT_FONTS_EMBED=0`: the fonts are files beside the program.\nconst BESIDE: bool = {beside};");
    }
    if beside && std::env::var_os("CRAFT_FONTS_DIR").is_none() {
        println!("cargo::warning=CRAFT_FONTS_EMBED=0 without CRAFT_FONTS_DIR: the manifest was not checked");
    }
    // Without them the app still runs and every language still selectable, so Chinese and Japanese
    // just paint boxes — the one failure a user cannot diagnose from what they see.
    if embedded == 0 && !beside && needed.is_none() {
        println!(
            "cargo::warning=no craft-fonts embedded: Chinese and Japanese text will render as empty boxes. \
             Clone https://github.com/storytold/craft-fonts next to this repo and rebuild with \
             CRAFT_FONTS_DIR=../craft-fonts"
        );
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("craft_fonts.rs");
    if let Err(e) = std::fs::write(&out, src) {
        println!("cargo::error=writing {}: {e}", out.display());
    }
}

/// Resolve a relative `CRAFT_FONTS_DIR` against both roots a caller might mean: a build script's
/// cwd is the *package* root (`crates/engine`), while the `../craft-fonts` the README and
/// craft-fonts' docs use is written relative to the *workspace* root and would otherwise resolve
/// to `crates/craft-fonts`.
fn resolve(dir: PathBuf) -> PathBuf {
    if dir.is_absolute() {
        return dir;
    }
    let package = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let workspace = package.parent().and_then(Path::parent).unwrap_or(package.as_path());
    let candidates = [workspace.join(&dir), package.join(&dir)];
    candidates.iter().find(|candidate| candidate.is_dir()).cloned().unwrap_or_else(|| candidates[0].clone())
}

/// One `CraftFont { .. }` initialiser per manifest line.
fn craft_fonts(dir: &Path, required: bool, needed: Option<&[&str]>) -> Result<String, String> {
    let manifest = dir.join("fonts/manifest.txt");
    println!("cargo::rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let root = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "wasm32");
    let mut out = String::new();
    let mut has_japanese_ui = false;
    let mut has_chinese_ui = false;
    let mut has_thai_ui = false;
    let mut has_hebrew_ui = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let [family, style, file, scripts, ..] = f.as_slice() else {
            return Err(format!("malformed manifest line: {line}"));
        };
        has_japanese_ui |= (*family, *style) == ("BIZ UDPGothic", "Regular");
        has_chinese_ui |= (*family, *style) == ("Noto Sans CJK SC", "Regular") && scripts.split(',').any(|script| script.trim() == "Hans");
        has_thai_ui |= (*family, *style) == ("Noto Sans Thai", "Regular") && scripts.split(',').any(|script| script.trim() == "Thai");
        has_hebrew_ui |= (*family, *style) == ("Noto Sans Hebrew", "Regular") && scripts.split(',').any(|script| script.trim() == "Hebr");
        if let Some(needed) = needed {
            let face: Vec<&str> = scripts.split(',').map(str::trim).collect();
            if !ui_languages::face_needed(needed, &face) {
                continue;
            }
        }
        if wasm && !matches!((*family, *style), ("BIZ UDPGothic" | "Noto Sans Thai" | "Noto Sans Georgian" | "Noto Sans Hebrew", "Regular")) {
            continue;
        }
        let path = dir.join(file).canonicalize().map_err(|e| format!("{file}: {e}"))?;
        // a font is a file of the checkout: a manifest entry or a link reaching outside it would
        // embed some other file of the build machine
        if !path.starts_with(&root) || !path.is_file() {
            return Err(format!("{file}: not a file inside {}", root.display()));
        }
        let shown = path.display().to_string();
        // one cargo instruction per line: a line break in the path would print a second one
        if shown.contains(['\n', '\r']) {
            return Err(format!("{file}: the path contains a line break"));
        }
        println!("cargo::rerun-if-changed={shown}");
        let scripts: Vec<String> = scripts.split(',').map(|s| format!("{:?}", s.trim())).collect();
        let _ = writeln!(
            out,
            "    CraftFont {{ family: {family:?}, style: {style:?}, scripts: &[{}], bytes: include_bytes!({:?}) }},",
            scripts.join(", "),
            path.display().to_string(),
        );
    }
    if required && !has_thai_ui {
        return Err("required Thai UI face Noto Sans Thai Regular (Thai) is missing from fonts/manifest.txt".into());
    }
    if required && !has_hebrew_ui {
        return Err("required Hebrew UI face Noto Sans Hebrew Regular (Hebr) is missing from fonts/manifest.txt".into());
    }
    if required && !has_japanese_ui {
        return Err("required Japanese UI face BIZ UDPGothic Regular is missing from fonts/manifest.txt".into());
    }
    if required && !wasm && !has_chinese_ui {
        return Err("required Simplified Chinese UI face Noto Sans CJK SC Regular (Hans) is missing from fonts/manifest.txt".into());
    }
    Ok(out)
}
