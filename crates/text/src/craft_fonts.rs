//! Fonts from craft-fonts (https://github.com/storytold/craft-fonts): the Japanese and Chinese
//! (and Arabic) faces the app ships when built with `CRAFT_FONTS_DIR`.
//!
//! This crate does not embed them itself: the app already embeds them once (the engine's build
//! script), and a second `include_bytes!` would put a second copy into the binary. Instead the
//! fonts are handed to this crate at runtime with [`install`] (the host passes its embedded
//! list), or read from a craft-fonts checkout with [`load_dir`] / [`install_from_env`] (tests,
//! tools). Every [`crate::FontDb`] created afterwards registers them as fallbacks. Every user of
//! the list must work when it is empty. Ported from PhotoCraft's `text::craft_fonts` (MIT).

use std::sync::{Arc, PoisonError, RwLock};

/// A craft-fonts face.
#[derive(Clone)]
pub struct CraftFont {
    pub family: String,
    pub style: String,
    /// ISO 15924 scripts the font is for, e.g. `"Jpan"`.
    pub scripts: Vec<String>,
    pub bytes: FontBytes,
}

/// Font file bytes: embedded in the binary, or loaded at runtime.
#[derive(Clone)]
pub enum FontBytes {
    Static(&'static [u8]),
    Shared(Arc<Vec<u8>>),
}

impl AsRef<[u8]> for FontBytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            FontBytes::Static(b) => b,
            FontBytes::Shared(b) => b.as_slice(),
        }
    }
}

impl std::fmt::Debug for CraftFont {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CraftFont").field("family", &self.family).field("style", &self.style).field("scripts", &self.scripts).finish()
    }
}

/// The preferred UI family for Japanese.
pub const UI_JAPANESE_FAMILY: &str = "BIZ UDPGothic";

impl CraftFont {
    /// True for a font meant for Japanese text.
    pub fn is_japanese(&self) -> bool {
        self.covers("Jpan")
    }

    /// Whether the font is meant for `script` (ISO 15924).
    pub fn covers(&self, script: &str) -> bool {
        self.scripts.iter().any(|s| s == script)
    }

    /// True for a Mincho (serif) face.
    pub fn is_mincho(&self) -> bool {
        self.family.contains("Mincho")
    }

    /// True for a font meant for any CJK script.
    pub fn is_cjk(&self) -> bool {
        self.scripts.iter().any(|s| matches!(s.as_str(), "Hans" | "Hant" | "Jpan" | "Kore" | "Hang" | "Hani"))
    }
}

static INSTALLED: RwLock<Vec<CraftFont>> = RwLock::new(Vec::new());

/// Makes `fonts` available to every [`crate::FontDb`] created afterwards (replaces earlier ones).
pub fn install(fonts: Vec<CraftFont>) {
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = fonts;
}

/// The installed craft fonts (empty unless [`install`] was called).
pub fn installed() -> Vec<CraftFont> {
    INSTALLED.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Largest font file [`load_dir`] reads (the biggest craft-fonts face is ~16 MB).
const MAX_FONT_FILE: u64 = 64 * 1024 * 1024;

/// Reads every font in a craft-fonts checkout's `fonts/manifest.txt`
/// (`family | style | file | scripts | ...` per line).
pub fn load_dir(dir: &std::path::Path) -> Result<Vec<CraftFont>, String> {
    let manifest = dir.join("fonts/manifest.txt");
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let [family, style, file, scripts, ..] = f.as_slice() else {
            return Err(format!("malformed manifest line: {line}"));
        };
        let path = dir.join(file);
        let len = std::fs::metadata(&path).map_err(|e| format!("{file}: {e}"))?.len();
        if len > MAX_FONT_FILE {
            return Err(format!("{file}: {len} bytes is too large for a font"));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{file}: {e}"))?;
        out.push(CraftFont {
            family: family.to_string(),
            style: style.to_string(),
            scripts: scripts.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            bytes: FontBytes::Shared(Arc::new(bytes)),
        });
    }
    Ok(out)
}

/// Loads and installs the checkout named by `CRAFT_FONTS_DIR` (a relative path resolves against
/// the current directory, then its ancestors). Returns how many fonts were installed; 0 when
/// the variable is unset or the checkout is unreadable.
pub fn install_from_env() -> usize {
    let Some(dir) = std::env::var_os("CRAFT_FONTS_DIR").filter(|d| !d.is_empty()).map(std::path::PathBuf::from) else {
        return 0;
    };
    let candidates: Vec<std::path::PathBuf> = if dir.is_absolute() {
        vec![dir]
    } else {
        std::env::current_dir().map(|cwd| cwd.ancestors().map(|a| a.join(&dir)).collect()).unwrap_or_default()
    };
    for c in candidates {
        if let Ok(fonts) = load_dir(&c) {
            let n = fonts.len();
            install(fonts);
            return n;
        }
    }
    0
}

/// The Japanese craft fonts in UI preference order: BIZ UDPGothic Regular first, then its other
/// styles, then the rest in manifest order.
pub fn japanese_for_ui() -> Vec<CraftFont> {
    let mut v: Vec<CraftFont> = installed().into_iter().filter(|f| f.is_japanese()).collect();
    v.sort_by_key(|f| (f.family != UI_JAPANESE_FAMILY, f.style != "Regular"));
    v
}

/// Japanese craft font families (each once) for document fallback: sans (Gothic) first, then
/// Mincho. Serif runs reorder them with [`mincho_first`].
pub fn japanese_families() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for f in japanese_for_ui() {
        if !v.contains(&f.family) {
            v.push(f.family);
        }
    }
    v.sort_by_key(|f| f.contains("Mincho"));
    v
}

/// Chinese (Simplified or Traditional) craft font families.
pub fn chinese_families() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for f in installed().into_iter().filter(|f| f.covers("Hans") || f.covers("Hant")) {
        if !v.contains(&f.family) {
            v.push(f.family);
        }
    }
    v
}

/// `fallback` with the Japanese craft Mincho families moved ahead of the craft Gothic one, in
/// place (the CJK script order around them is unchanged). For serif runs.
pub fn mincho_first(fallback: &[String]) -> Vec<String> {
    let craft = japanese_families();
    let slots: Vec<usize> = fallback.iter().enumerate().filter(|(_, f)| craft.contains(f)).map(|(i, _)| i).collect();
    let mut group: Vec<String> = slots.iter().filter_map(|&i| fallback.get(i).cloned()).collect();
    group.sort_by_key(|f| !f.contains("Mincho"));
    let mut out = fallback.to_vec();
    for (slot, fam) in slots.into_iter().zip(group) {
        if let Some(o) = out.get_mut(slot) {
            *o = fam;
        }
    }
    out
}

/// A family name that reads as serif (Mincho, Song/Ming, Myeongjo, Times …): its Japanese
/// fallback should be a Mincho face rather than a Gothic one.
pub fn is_serif_family(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.contains("sans") || n.contains("gothic") {
        return false;
    }
    const SERIF: &[&str] = &[
        "serif",
        "mincho",
        "minchō",
        "song",
        "ming",
        "myeongjo",
        "myungjo",
        "batang",
        "times",
        "georgia",
        "garamond",
        "minion",
        "baskerville",
        "caslon",
        "bodoni",
        "didot",
        "palatino",
        "cambria",
        "book antiqua",
        "century",
        "hoefler",
    ];
    SERIF.iter().any(|s| n.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mincho_first_reorders_only_craft_families() {
        install_from_env();
        let fams = japanese_families();
        if fams.len() < 2 {
            eprintln!("skipping: built without craft-fonts (or with only one Japanese family)");
            return;
        }
        let fallback: Vec<String> =
            ["Noto Sans"].iter().map(|s| s.to_string()).chain(fams.iter().cloned()).chain(["Hiragino Sans".to_string()]).collect();
        let serif = mincho_first(&fallback);
        assert_eq!(serif.first().map(String::as_str), Some("Noto Sans"));
        assert_eq!(serif.last().map(String::as_str), Some("Hiragino Sans"));
        assert!(serif.get(1).is_some_and(|f| f.contains("Mincho")), "{serif:?}");
        assert_eq!(serif.len(), fallback.len());
    }

    #[test]
    fn load_dir_reads_a_manifest_and_rejects_bad_ones() {
        let dir = std::env::temp_dir().join(format!("dac-text-craft-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("fonts/x")).unwrap();
        std::fs::write(dir.join("fonts/x/A.ttf"), crate::fonts::INTER_REGULAR).unwrap();
        std::fs::write(dir.join("fonts/manifest.txt"), "# c\n\nFake Mincho | Regular | fonts/x/A.ttf | Jpan,Latn | OFL-1.1 | x | y | z\n").unwrap();
        let fonts = load_dir(&dir).unwrap();
        assert_eq!(fonts.len(), 1);
        assert!(fonts[0].is_japanese() && fonts[0].is_mincho() && fonts[0].is_cjk());
        std::fs::write(dir.join("fonts/manifest.txt"), "only | two\n").unwrap();
        assert!(load_dir(&dir).is_err());
        std::fs::write(dir.join("fonts/manifest.txt"), "A | B | fonts/missing.ttf | Jpan\n").unwrap();
        assert!(load_dir(&dir).is_err());
        assert!(load_dir(&dir.join("nope")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mincho_first_without_craft_fonts_is_identity() {
        let fallback = vec!["Noto Sans".to_string(), "Hiragino Sans".to_string()];
        assert_eq!(mincho_first(&fallback), fallback);
    }

    #[test]
    fn serif_names() {
        for s in ["Shippori Mincho", "Times New Roman", "Noto Serif JP", "Georgia", "Songti SC", "Adobe Garamond Pro"] {
            assert!(is_serif_family(s), "{s}");
        }
        for s in ["Inter", "Noto Sans Serif", "BIZ UDPGothic", "Helvetica", "", "Noto Sans CJK JP"] {
            assert!(!is_serif_family(s), "{s}");
        }
    }

    #[test]
    fn ui_order_prefers_biz_udpgothic_regular() {
        install_from_env();
        let v = japanese_for_ui();
        let Some(first) = v.first() else {
            eprintln!("skipping: built without craft-fonts (CRAFT_FONTS is empty)");
            return;
        };
        assert_eq!((first.family.as_str(), first.style.as_str()), (UI_JAPANESE_FAMILY, "Regular"));
        assert!(v.iter().all(|f| f.is_japanese() && !f.bytes.as_ref().is_empty()));
    }
}
