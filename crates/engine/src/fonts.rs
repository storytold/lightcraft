//! Fonts from the optional craft-fonts build input (storytold/craft-fonts).
//!
//! Built with `CRAFT_FONTS_DIR=<craft-fonts checkout>`, [`CRAFT_FONTS`] holds every font in its
//! manifest (today: the Japanese and Simplified-Chinese UI and document fonts); otherwise it is
//! empty and LightCraft uses only its own bundled fonts (Inter). The web build embeds none: its
//! page loads the faces from files beside the WASM and passes them to [`add_fonts`] before start-up.
//! Both the egui UI and the export watermark renderer read [`craft_fonts`]. See craftrules
//! `standards/fonts.md`.

use std::sync::OnceLock;

/// A font from the optional craft-fonts build input (empty unless built with `CRAFT_FONTS_DIR`).
#[derive(Clone, Copy)]
pub struct CraftFont {
    pub family: &'static str,
    pub style: &'static str,
    /// ISO 15924 scripts the font is for, e.g. `"Jpan"`.
    pub scripts: &'static [&'static str],
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/craft_fonts.rs"));

/// [`CRAFT_FONTS`] plus the faces given to [`add_fonts`].
static ALL: OnceLock<Vec<CraftFont>> = OnceLock::new();

/// The craft-fonts faces this instance has: the embedded [`CRAFT_FONTS`], plus the faces the web
/// page loaded beside the WASM ([`add_fonts`]).
pub fn craft_fonts() -> &'static [CraftFont] {
    ALL.get().map_or(CRAFT_FONTS, Vec::as_slice)
}

/// Add faces loaded at run time (the web build's font files), before anything reads
/// [`craft_fonts`]; only the first call counts. A face that does not parse is left out and
/// returned, so a host that answers a missing file with an HTML page cannot break the UI.
pub fn add_fonts(fonts: Vec<CraftFont>) -> Vec<CraftFont> {
    let (good, bad) = parsable(fonts);
    let _ = ALL.set(CRAFT_FONTS.iter().copied().chain(good).collect());
    bad
}

/// `fonts` split into the faces that parse and those that do not.
fn parsable(fonts: Vec<CraftFont>) -> (Vec<CraftFont>, Vec<CraftFont>) {
    fonts.into_iter().partition(|f| ab_glyph::FontRef::try_from_slice(f.bytes).is_ok())
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
        if CRAFT_FONTS.is_empty() {
            eprintln!("built without CRAFT_FONTS_DIR: no craft-fonts to check");
            return;
        }
        for f in CRAFT_FONTS {
            assert!(!f.family.is_empty() && !f.style.is_empty() && !f.scripts.is_empty(), "{}", f.family);
            assert!(ab_glyph::FontRef::try_from_slice(f.bytes).is_ok(), "{} {} parses", f.family, f.style);
        }
        assert!(japanese(CRAFT_FONTS).next().is_some(), "craft-fonts carries Japanese faces");
        // Every script face that ships is a UI fallback, and has the glyphs of its script (#736).
        use ab_glyph::Font;
        for (script, text) in [("Thai", "ภาพถ่ายสวัสดี๑๒๓"), ("Arab", "صورة"), ("Geor", "ფოტო")] {
            let Some(face) = ui_fallback(CRAFT_FONTS, "Latn", "Regular").into_iter().find(|f| f.covers(script)) else { continue };
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

    #[test]
    fn added_faces_that_do_not_parse_are_refused() {
        // e.g. a host answering a missing font file with its index page (status 200)
        static INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let face = |family, bytes| CraftFont { family, style: "Regular", scripts: &["Geor"], bytes };
        let (good, bad) = parsable(vec![face("Good Sans", INTER), face("Not A Font", b"<!doctype html>"), face("Empty", b"")]);
        assert_eq!(good.iter().map(|f| f.family).collect::<Vec<_>>(), ["Good Sans"]);
        assert_eq!(bad.iter().map(|f| f.family).collect::<Vec<_>>(), ["Not A Font", "Empty"]);
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
