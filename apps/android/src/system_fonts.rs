//! Fallback faces from the device's own fonts (`/system/fonts`), for scripts craft-fonts didn't
//! bring: Korean, Thai, Devanagari, … without them, names in those scripts show boxes. They are
//! read at start-up, never bundled, and listed after every other face (`theme::set_system_fallbacks`).
//! Memory is bounded: a few faces, each and all together under a size cap. A face that can't be
//! read is logged and skipped.

use std::path::Path;
use std::sync::Arc;

const DIR: &str = "/system/fonts";
/// The most faces taken.
const MAX_FACES: usize = 12;
/// A larger file is skipped.
const MAX_FILE: u64 = 32 << 20;
/// All faces together stay under this.
const MAX_TOTAL: u64 = 64 << 20;
/// The most directory entries looked at.
const MAX_ENTRIES: usize = 2000;

/// File-name stems (after `NotoSans`, `NotoSerif` …) and the ISO 15924 script each is for.
const SCRIPTS: &[(&str, &str)] = &[
    ("Thai", "Thai"),
    ("Arabic", "Arab"),
    ("NaskhArabic", "Arab"),
    ("Hebrew", "Hebr"),
    ("Devanagari", "Deva"),
    ("Bengali", "Beng"),
    ("Tamil", "Taml"),
    ("Telugu", "Telu"),
    ("Kannada", "Knda"),
    ("Malayalam", "Mlym"),
    ("Gujarati", "Gujr"),
    ("Gurmukhi", "Guru"),
    ("Sinhala", "Sinh"),
    ("Khmer", "Khmr"),
    ("Lao", "Laoo"),
    ("Myanmar", "Mymr"),
    ("Armenian", "Armn"),
    ("Georgian", "Geor"),
    ("Ethiopic", "Ethi"),
];

/// Reads the fallback faces and hands them to the UI's font definitions.
pub fn install() {
    let faces = load(Path::new(DIR), &covered_scripts());
    log::info!("{} system fallback fonts", faces.len());
    lcc_ui_egui::theme::set_system_fallbacks(faces);
}

/// The scripts craft-fonts already covers (a system CJK face is still taken unless Korean is among
/// them too: one system CJK face covers Chinese, Japanese and Korean).
fn covered_scripts() -> Vec<&'static str> {
    lcc_engine::craft_fonts().iter().flat_map(|f| f.scripts.iter().copied()).collect()
}

/// The script a system font file is for, when it is one we take.
fn script_of(file: &str) -> Option<&'static str> {
    let stem = file.strip_suffix(".ttf").or_else(|| file.strip_suffix(".otf")).or_else(|| file.strip_suffix(".ttc"))?;
    if stem.starts_with("NotoSansCJK") && stem.contains("Regular") {
        return Some("CJK");
    }
    if stem.starts_with("DroidSansFallback") {
        return Some("CJK-old");
    }
    let rest = stem.strip_prefix("NotoSans")?;
    let (name, style) = rest.split_once('-')?;
    if style != "Regular" && style != "VF" {
        return None;
    }
    // `NotoSansThaiUI` is the tighter UI variant of the same script
    let name = name.strip_suffix("UI").unwrap_or(name);
    SCRIPTS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

fn load(dir: &Path, covered: &[&str]) -> Vec<(String, Arc<egui::FontData>)> {
    let cjk_covered = ["Hans", "Jpan", "Kore"].iter().all(|s| covered.contains(s));
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("system fonts {}: {e}", dir.display());
            return Vec::new();
        }
    };
    let mut files: Vec<(String, &'static str, u64)> = Vec::new();
    for entry in entries.take(MAX_ENTRIES).flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(script) = script_of(&name) else { continue };
        let skip = match script {
            "CJK" | "CJK-old" => cjk_covered,
            s => covered.contains(&s),
        };
        if skip {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_file() && meta.len() <= MAX_FILE {
            files.push((name, script, meta.len()));
        }
    }
    // one face per script: the UI variant or the smallest; the old CJK fallback only without Noto CJK
    files.sort_by(|a, b| (a.1, !a.0.contains("UI"), a.2, &a.0).cmp(&(b.1, !b.0.contains("UI"), b.2, &b.0)));
    files.dedup_by(|b, a| a.1 == b.1);
    if files.iter().any(|f| f.1 == "CJK") {
        files.retain(|f| f.1 != "CJK-old");
    }
    let mut out = Vec::new();
    let mut total = 0u64;
    for (name, _, len) in files {
        if out.len() >= MAX_FACES || total.saturating_add(len) > MAX_TOTAL {
            break;
        }
        match std::fs::read(dir.join(&name)) {
            // egui panics on a face it can't parse: check first
            Ok(bytes) if skrifa::FontRef::from_index(&bytes, 0).is_err() => log::warn!("system font {name}: not a font egui can read"),
            Ok(bytes) => {
                total = total.saturating_add(bytes.len() as u64);
                out.push((format!("system {name}"), Arc::new(egui::FontData::from_owned(bytes))));
            }
            Err(e) => log::warn!("system font {name}: {e}"),
        }
    }
    out
}
