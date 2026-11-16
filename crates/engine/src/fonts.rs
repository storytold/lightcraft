//! Fonts from the optional craft-fonts build input (storytold/craft-fonts).
//!
//! Built with `CRAFT_FONTS_DIR=<craft-fonts checkout>`, [`craft_fonts`] holds every font in its
//! manifest (today: the Japanese and Simplified-Chinese UI and document fonts); otherwise it is
//! empty and the app uses only its own bundled fonts (Inter). Both the egui UI and the export
//! watermark renderer read it from here. Font rules: `AGENTS.md` → Assets (OFL, unmodified,
//! credited in assets/ATTRIBUTION.md).
//!
//! The fonts are embedded in the binary, unless it was built with `CRAFT_FONTS_EMBED=0`: then the
//! package carries them as files, once for the app and the CLI beside it (the macOS bundle's
//! `Contents/Resources/fonts/`, the checkout's `fonts/` layout with its manifest), and they are
//! read on first use. A file that is missing or damaged leaves its face out and is reported
//! ([`craft_font_problems`]); the rest still load, and Latin text never needs them (Inter).

#[cfg(not(target_arch = "wasm32"))]
pub mod package;
#[cfg(not(target_arch = "wasm32"))]
pub use package::{LoadedFace, read_package};

/// A font from the optional craft-fonts build input (empty unless built with `CRAFT_FONTS_DIR`).
pub struct CraftFont {
    pub family: &'static str,
    pub style: &'static str,
    /// ISO 15924 scripts the font is for, e.g. `"Jpan"`.
    pub scripts: &'static [&'static str],
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/craft_fonts.rs"));

/// The craft-fonts faces: embedded, or read once from the package's files (built with
/// `CRAFT_FONTS_EMBED=0`). Empty without craft-fonts; every user must work with an empty list.
pub fn craft_fonts() -> &'static [CraftFont] {
    #[cfg(not(target_arch = "wasm32"))]
    if BESIDE {
        return package::loaded().fonts;
    }
    EMBEDDED
}

/// Faces the package should carry but that could not be read (always empty for embedded fonts).
/// Each one is also logged as a warning when the fonts are read.
pub fn craft_font_problems() -> &'static [FontProblem] {
    #[cfg(not(target_arch = "wasm32"))]
    if BESIDE {
        return &package::loaded().problems;
    }
    &[]
}

/// A craft-fonts face the package lists but that could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontProblem {
    /// What is affected, for people: the face (`"Noto Sans Arabic Regular"`), or the font list's
    /// path when the list itself could not be read.
    pub what: String,
    /// What went wrong (English, for the log).
    pub detail: String,
}

impl std::fmt::Display for FontProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.what, self.detail)
    }
}

impl CraftFont {
    /// Whether the font is meant for `script` (ISO 15924, e.g. `"Jpan"`).
    pub fn covers(&self, script: &str) -> bool {
        self.scripts.contains(&script)
    }

    /// Mincho (serif) faces, the preferred Japanese faces for document-like text.
    pub fn is_mincho(&self) -> bool {
        self.family.contains("Mincho")
    }

    /// Whether the font can stand in for `script`: it is meant for it, or — until craft-fonts has a
    /// Traditional Chinese face — it is a Simplified Chinese face, which carries the Traditional
    /// characters too (with mainland glyph forms; still closer than the Japanese faces).
    pub fn serves(&self, script: &str) -> bool {
        self.covers(script) || (script == "Hant" && self.covers("Hans"))
    }
}

/// The craft-fonts faces for Japanese, in `fonts`' order (empty without craft-fonts).
pub fn japanese(fonts: &'static [CraftFont]) -> impl Iterator<Item = &'static CraftFont> {
    fonts.iter().filter(|f| f.covers("Jpan"))
}

/// The craft-fonts faces for any CJK script: what any CJK text needs, regardless of language.
pub fn cjk(fonts: &'static [CraftFont]) -> impl Iterator<Item = &'static CraftFont> {
    fonts.iter().filter(|f| f.scripts.iter().any(|script| is_cjk_script(script)))
}

/// Whether an ISO 15924 script is written with Han, kana or hangul characters (Unicode's CJK
/// scripts), which Inter has no glyphs for.
pub fn is_cjk_script(script: &str) -> bool {
    matches!(script, "Hans" | "Hant" | "Jpan" | "Kore" | "Hang" | "Hani" | "Bopo" | "Yiii" | "Nshu" | "Tang")
}

/// The craft-fonts faces to fall back on for `script`, in preference order: the CJK faces, those of
/// that script first (for `style` when asked), then the faces of every other non-Latin script
/// (Thai, Arabic, ...), which file and folder names can contain whatever the interface language.
/// One CJK face list serves every language, but Han characters are shared between them: a Chinese
/// reader must get the Chinese forms and a Japanese reader the Japanese ones, so the active
/// language's own script comes first. The other scripts share no characters, so their order only
/// prefers `style`. Empty without craft-fonts.
pub fn ui_fallback<'a>(fonts: &'a [CraftFont], script: &str, style: &str) -> Vec<&'a CraftFont> {
    let mut faces = cjk_fallback(fonts, script, style);
    let mut others: Vec<&CraftFont> = fonts.iter().filter(|font| !covers_cjk(font) && font.scripts.iter().any(|s| *s != "Latn")).collect();
    others.sort_by_key(|font| font.style != style);
    faces.extend(others);
    faces
}

/// The CJK part of [`ui_fallback`], in the same preference order.
pub fn cjk_fallback<'a>(fonts: &'a [CraftFont], script: &str, style: &str) -> Vec<&'a CraftFont> {
    let mut faces: Vec<&CraftFont> = fonts.iter().filter(|font| covers_cjk(font)).collect();
    faces.sort_by_key(|font| {
        (
            // The language's own script first. A face dedicated to the script is preferred over a
            // multi-script face (Noto Sans CJK SC claims `Hans,Latn`; a Japanese face claims `Jpan`).
            !font.serves(script),
            font.scripts.len() != 1,
            font.family != "BIZ UDPGothic",
            font.style != style,
            font.is_mincho(),
        )
    });
    faces
}

/// Whether the font is for any CJK script.
fn covers_cjk(font: &CraftFont) -> bool {
    font.scripts.iter().any(|script| is_cjk_script(script))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn craft_fonts_are_well_formed_or_absent() {
        if craft_fonts().is_empty() {
            eprintln!("built without CRAFT_FONTS_DIR: no craft-fonts to check");
            return;
        }
        for f in craft_fonts() {
            assert!(!f.family.is_empty() && !f.style.is_empty() && !f.scripts.is_empty(), "{}", f.family);
            assert!(ab_glyph::FontRef::try_from_slice(f.bytes).is_ok(), "{} {} parses", f.family, f.style);
        }
        assert!(japanese(craft_fonts()).next().is_some(), "craft-fonts carries Japanese faces");
        // Every script face that ships is a UI fallback, and has the glyphs of its script (#736).
        use ab_glyph::Font;
        for (script, text) in [("Thai", "ภาพถ่ายสวัสดี๑๒๓"), ("Arab", "صورة"), ("Geor", "ფოტო"), ("Hebr", "תמונות טנזניה ךםןףץ־׳״ָׁ₪")]
        {
            let Some(face) = ui_fallback(craft_fonts(), "Latn", "Regular").into_iter().find(|f| f.covers(script)) else { continue };
            let font = ab_glyph::FontRef::try_from_slice(face.bytes).expect("parses");
            assert!(text.chars().all(|c| font.glyph_id(c).0 != 0), "{} draws {script}", face.family);
        }
    }

    /// Each language's own faces come first, so shared Han keeps the language's forms, and Latin
    /// text still reaches Inter before any CJK fallback.
    #[test]
    fn a_language_gets_its_own_script_first() {
        static FACES: &[CraftFont] = &[
            CraftFont { family: "BIZ UDPGothic", style: "Regular", scripts: &["Jpan", "Latn"], bytes: &[] },
            CraftFont { family: "Noto Sans CJK SC", style: "Regular", scripts: &["Hans", "Latn"], bytes: &[] },
            CraftFont { family: "BIZ UDMincho", style: "Regular", scripts: &["Jpan", "Latn"], bytes: &[] },
        ];
        let first = |script: &str| cjk_fallback(FACES, script, "Regular").first().map(|f| f.family);
        assert_eq!(first("Hans"), Some("Noto Sans CJK SC"));
        assert_eq!(first("Jpan"), Some("BIZ UDPGothic"));
        // No Traditional Chinese face yet: the Chinese face stands in before the Japanese ones.
        assert_eq!(first("Hant"), Some("Noto Sans CJK SC"));
        // Every CJK face is still offered to every language.
        assert_eq!(cjk_fallback(FACES, "Hans", "Regular").len(), 3);
        assert_eq!(cjk_fallback(&[], "Hans", "Regular").len(), 0);
    }

    /// Faces for other scripts (#736: Thai folder names showed boxes) follow the CJK faces in the
    /// UI fallback whatever the language; a Latin-only face is not a fallback (Inter covers Latin).
    #[test]
    fn other_scripts_follow_the_cjk_faces() {
        static FACES: &[CraftFont] = &[
            CraftFont { family: "Noto Sans Thai", style: "Regular", scripts: &["Thai", "Latn"], bytes: &[] },
            CraftFont { family: "Latin Only Sans", style: "Regular", scripts: &["Latn"], bytes: &[] },
            CraftFont { family: "Noto Sans CJK SC", style: "Regular", scripts: &["Hans", "Latn"], bytes: &[] },
            CraftFont { family: "Noto Sans Arabic", style: "Bold", scripts: &["Arab"], bytes: &[] },
            CraftFont { family: "Noto Sans Arabic", style: "Regular", scripts: &["Arab"], bytes: &[] },
        ];
        let families = |script: &str, style: &str| ui_fallback(FACES, script, style).iter().map(|f| (f.family, f.style)).collect::<Vec<_>>();
        assert_eq!(
            families("Latn", "Regular"),
            [("Noto Sans CJK SC", "Regular"), ("Noto Sans Thai", "Regular"), ("Noto Sans Arabic", "Regular"), ("Noto Sans Arabic", "Bold")]
        );
        assert_eq!(families("Hans", "Bold")[0], ("Noto Sans CJK SC", "Regular"));
        assert_eq!(families("Hans", "Bold")[1], ("Noto Sans Arabic", "Bold"));
        assert!(ui_fallback(&[], "Thai", "Regular").is_empty());
    }
}
