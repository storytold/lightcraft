//! The single-language build table (`CRAFT_FONTS_LANGUAGE=<code>`, issue #1231): for every UI
//! language, the ISO 15924 scripts whose craft-fonts faces that language's UI needs. Latin,
//! Cyrillic and Greek languages need none (the bundled Inter covers them).
//!
//! Shared by `build.rs` (which embeds only the matching faces) and the library (whose tests check
//! every shipped catalog in `crates/ui-egui/locales/` has a row), so it uses nothing but `core`.

/// `(code, scripts)`: one row per UI language, codes as `crates/ui-egui/locales/<code>.json`.
pub const UI_LANGUAGE_FONTS: &[(&str, &[&str])] = &[
    ("en", &[]),
    ("zh-hans", &["Hans"]),
    // craft-fonts has no Traditional face yet: the Simplified one stands in (`CraftFont::serves`)
    ("zh-hant", &["Hant", "Hans"]),
    ("ja", &["Jpan"]),
    ("pt-br", &[]),
    ("es", &[]),
    ("de", &[]),
    ("ru", &[]),
    ("fr", &[]),
    ("uk", &[]),
    ("pl", &[]),
    ("cs", &[]),
    ("it", &[]),
    ("ka", &["Geor"]),
    ("sv", &[]),
    ("fi", &[]),
    ("id", &[]),
    ("vi", &[]),
];

/// The scripts `code`'s UI needs, or `None` for a code the table doesn't list.
pub fn scripts_for(code: &str) -> Option<&'static [&'static str]> {
    UI_LANGUAGE_FONTS.iter().find(|(c, _)| *c == code).map(|(_, scripts)| *scripts)
}

/// Whether a face for `face_scripts` is embedded for a UI that needs `needed`.
pub fn face_needed(needed: &[&str], face_scripts: &[&str]) -> bool {
    face_scripts.iter().any(|script| needed.contains(script))
}

/// Every code in the table, comma-separated, for error messages.
pub fn valid_codes() -> String {
    UI_LANGUAGE_FONTS.iter().map(|(c, _)| *c).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new catalog can't ship without a row here: the single-language build would reject its code.
    #[test]
    fn every_shipped_locale_has_a_row() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui-egui/locales");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let Some(code) = name.strip_suffix(".json") else { continue };
            if code.ends_with("-formats") {
                continue;
            }
            seen += 1;
            assert!(scripts_for(code).is_some(), "locales/{name} has no row in UI_LANGUAGE_FONTS");
        }
        assert!(seen > 0, "no catalogs found in {}", dir.display());
        assert!(scripts_for("en").is_some());
    }

    #[test]
    fn latin_only_build_keeps_no_other_script_faces() {
        let needed = scripts_for("de").unwrap();
        assert!(!face_needed(needed, &["Jpan", "Latn"]));
        assert!(!face_needed(needed, &["Thai", "Latn"]));
        assert!(face_needed(scripts_for("ja").unwrap(), &["Jpan", "Latn"]));
        assert!(!face_needed(scripts_for("ja").unwrap(), &["Hans", "Latn"]));
        // zh-hant draws Traditional text with the Simplified face until craft-fonts has a Traditional one
        assert!(face_needed(scripts_for("zh-hant").unwrap(), &["Hans", "Latn"]));
        assert!(scripts_for("xx").is_none());
        assert!(valid_codes().contains("zh-hant"));
    }
}
