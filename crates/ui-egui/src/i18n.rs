//! Presentation-only localization. Command ids, user names and catalog data remain unchanged.
use std::{cell::Cell, collections::BTreeMap, sync::OnceLock};

use serde::{Deserialize, Serialize};

include!(concat!(env!("OUT_DIR"), "/ja-formats.rs"));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ja")]
    Ja,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::En, Self::Ja];
    pub fn name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ja => "日本語",
        }
    }
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "ja" => Some(Self::Ja),
            _ => None,
        }
    }
    pub fn tr(self, source: &str) -> &str {
        if self == Self::Ja { japanese().get(source).map(String::as_str).unwrap_or(source) } else { source }
    }
}

thread_local! {
    static LANGUAGE: Cell<Language> = const { Cell::new(Language::En) };
}

pub fn default_language() -> Language {
    if std::env::var("LIGHTCRAFT_LANGUAGE").as_deref() == Ok("ja") { Language::Ja } else { Language::En }
}

pub fn set_language(language: Language) {
    LANGUAGE.with(|value| value.set(language));
}

pub fn is_japanese() -> bool {
    LANGUAGE.with(|value| value.get() == Language::Ja)
}

fn japanese() -> &'static BTreeMap<String, String> {
    static MESSAGES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    MESSAGES.get_or_init(|| {
        serde_json::from_str(include_str!("../locales/ja.json")).unwrap_or_else(|error| {
            log::error!("Invalid Japanese message catalog: {error}");
            BTreeMap::new()
        })
    })
}

/// Translate a built-in display label, preserving unknown labels verbatim.
/// Never call this on editable user text, filenames or command identifiers.
pub fn tr(source: &str) -> &str {
    if is_japanese() { japanese().get(source).map(String::as_str).unwrap_or(source) } else { source }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_valid_and_contains_core_workflows() {
        let messages: BTreeMap<String, String> = serde_json::from_str(include_str!("../locales/ja.json")).unwrap();
        for key in [
            "Import Photos…",
            "Export…",
            "Exposure",
            "White Balance",
            "Settings",
            "Language",
            "Select photo",
            "Candidate",
            "Auto settings applied",
            "Estimated camera colour",
            "Camera colour (macOS)",
        ] {
            assert!(messages.get(key).is_some_and(|value| !value.is_empty() && value != key), "{key}");
        }
    }

    #[test]
    fn language_switches_and_unknown_text_survives() {
        set_language(Language::Ja);
        assert_eq!(tr("Exposure"), "露出");
        assert_eq!(tr("my-photo.jpg"), "my-photo.jpg");
        assert_eq!(tr("develop.set"), "develop.set");
        assert_eq!(crate::menubar::display_item_label("album.addPhotos", &serde_json::json!({"id": 1}), "Color"), "Color");
        assert_eq!(crate::menubar::display_item_label("app.export", &serde_json::json!({"preset": "Color"}), "Color"), "Color");
        assert_eq!(crate::menubar::display_item_label("view.photoGrid", &serde_json::Value::Null, "Color"), "カラー");
        set_language(Language::En);
        assert_eq!(tr("Exposure"), "Exposure");
    }

    #[test]
    fn translated_formats_preserve_counts_and_remove_english_plural_suffixes() {
        set_language(Language::Ja);
        assert_eq!(tr_format!("{n} photo{}", "s", n = 12), "12枚");
        assert_eq!(tr_format!("Exported {ok} of {total} photo{}", "s", ok = 4, total = 12), "12枚中4枚を書き出しました");
        set_language(Language::En);
        assert_eq!(tr_format!("{n} photo{}", "s", n = 12), "12 photos");
    }

    #[test]
    fn preferences_round_trip_and_old_settings_remain_readable() {
        let old: crate::state::UiState = serde_json::from_str("{}").unwrap();
        assert_eq!(old.language, Language::En);
        let settings = crate::state::UiState { language: Language::Ja, ..old };
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.language, Language::Ja);
    }

    #[test]
    fn japanese_is_painted_and_both_font_weights_cover_the_catalog() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        app.ui.language = Language::Ja;
        app.ui.left_panel = true;
        let mut text = String::new();
        fn collect(shape: &egui::epaint::Shape, text: &mut String) {
            match shape {
                egui::epaint::Shape::Text(shape) => {
                    text.push_str(&shape.galley.job.text);
                    text.push('\n');
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect(shape, text)),
                _ => {}
            }
        }
        for frame in 0..4 {
            let input = crate::headless::HeadlessView::raw_input(egui::vec2(1600.0, 1000.0), 1.0, frame as f64 / 60.0, vec![]);
            let mut out = ctx.run_ui(input, |ui| {
                app.logic(ui.ctx());
                app.ui(ui);
            });
            // This assertion inspects shapes without a renderer; discard texture uploads explicitly.
            out.textures_delta.clear();
            text.clear();
            for shape in out.shapes {
                collect(&shape.shape, &mut text);
            }
        }
        assert!(text.contains("マイフォト"), "{text}");
        assert!(text.contains("すべての写真"), "{text}");
        ctx.fonts_mut(|fonts| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                let font = egui::FontId::new(13.0, family);
                for message in japanese().values() {
                    for ch in message.chars().filter(|ch| !ch.is_whitespace()) {
                        assert!(fonts.has_glyph(&font, ch), "Missing glyph {ch} in {message}");
                    }
                }
            }
        });
        // Locale affects presentation only: command ids remain the same.
        let ids = |app: &crate::LightcraftApp| crate::menus::menu_entries(app).into_iter().map(|entry| entry.id).collect::<Vec<_>>();
        let japanese_ids = ids(&app);
        set_language(Language::En);
        assert_eq!(japanese_ids, ids(&app));
    }
}
