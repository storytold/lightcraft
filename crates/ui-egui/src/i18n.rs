//! Presentation-only localization. Command ids, user names and catalog data remain unchanged.
//!
//! A language is one entry in [`language_table!`] below: its BCP-47 code, the endonym shown in the
//! Language menus, the ISO 15924 script its text needs (which picks the CJK font fallback), and its
//! message catalog (`locales/<code>.json`, embedded at build time). Adding a language is that entry
//! plus its catalogs — every menu and settings surface is built from the table, so nothing else in
//! the UI changes.

use std::{cell::RefCell, collections::BTreeMap, sync::OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The language table.
///
/// The source language comes first and alone: it is the text the UI is authored in, which every
/// other language translates. `name` is written in the language itself (an endonym, so a user who
/// cannot read the current UI language can still find their own).
macro_rules! language_table {
    ($source:ident, $source_code:literal; $( $variant:ident, $code:literal, $name:literal, $script:literal, $catalog:expr );* $(;)?) => {
        /// A UI language.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Locale {
            /// The language the UI is written in.
            #[default]
            $source,
            $($variant,)*
        }

        impl Locale {
            /// Every language, in menu order (the source language first).
            pub const ALL: &'static [Self] = &[Self::$source, $(Self::$variant,)*];

            pub const fn code(self) -> &'static str {
                match self { Self::$source => $source_code, $(Self::$variant => $code,)* }
            }

            /// The language's own name, as shown in the Language menus.
            pub const fn name(self) -> &'static str {
                match self { Self::$source => "English", $(Self::$variant => $name,)* }
            }

            /// The ISO 15924 script this language is written in (`"Latn"`, `"Jpan"`, `"Hans"`…):
            /// the craft-fonts faces for it are the ones this language needs.
            pub const fn script(self) -> &'static str {
                match self { Self::$source => "Latn", $(Self::$variant => $script,)* }
            }

            /// The language's embedded catalog, parsed once. The source language has none: its
            /// lookups return the source text itself.
            fn catalog(self) -> &'static BTreeMap<String, String> {
                static CATALOGS: OnceLock<BTreeMap<Locale, BTreeMap<String, String>>> = OnceLock::new();
                static EMPTY: OnceLock<BTreeMap<String, String>> = OnceLock::new();
                // A malformed catalog logs and comes back empty: a broken translation degrades to
                // English text, it never takes the app down.
                let parse = |catalog: Option<&'static str>| match catalog {
                    Some(json) => match serde_json::from_str(json) {
                        Ok(messages) => messages,
                        Err(error) => {
                            log::error!("Invalid message catalog for {}: {error}", self.code());
                            BTreeMap::new()
                        }
                    },
                    None => BTreeMap::new(),
                };
                CATALOGS
                    .get_or_init(|| {
                        Locale::ALL
                            .iter()
                            .map(|language| {
                                let catalog = match *language { $(Self::$variant => Some($catalog),)* _ => None };
                                (*language, parse(catalog))
                            })
                            .collect()
                    })
                    .get(&self)
                    .unwrap_or_else(|| EMPTY.get_or_init(BTreeMap::new))
            }

            pub fn parse(code: &str) -> Option<Self> {
                match code { $source_code => Some(Self::$source), $($code => Some(Self::$variant),)* _ => None }
            }

            /// Lenient lookup: `zh`, `zh-CN`, `zh_Hans`, `en-US`, `ja_JP.UTF-8`… all reach a
            /// shipped language. A region or script the table does not ship falls back to the
            /// language itself, so a user is never left with no language at all.
            pub fn parse_tag(tag: &str) -> Option<Self> {
                let tag = tag.split(['.', '@']).next().unwrap_or(tag).replace('_', "-");
                let lower = tag.to_ascii_lowercase();
                if let Some(exact) = Self::ALL.iter().find(|language| language.code().eq_ignore_ascii_case(&lower)) {
                    return Some(*exact);
                }
                let mut parts = lower.split('-');
                let language = parts.next()?;
                // `zh-Hans-CN`, `zh-CN` and `zh` all name the same base language.
                let script = parts.next().and_then(|part| match part {
                    "hans" | "cn" | "sg" => Some("Hans"),
                    "hant" | "tw" | "hk" | "mo" => Some("Hant"),
                    _ => None,
                });
                Self::ALL
                    .iter()
                    .find(|candidate| {
                        let base = candidate.code().split('-').next().unwrap_or_default();
                        base == language && script.is_none_or(|script| candidate.script() == script)
                    })
                    .copied()
            }

            pub fn tr(self, source: &str) -> &str {
                tr_in(self, source)
            }
        }
    };
}

// The source language, then one entry per translation: code, endonym, ISO 15924 script, embedded
// catalog (`locales/<code>.json`).
language_table! {
    En, "en";
    ZhHans, "zh-hans", "简体中文", "Hans", include_str!("../locales/zh-hans.json");
    ZhHant, "zh-hant", "繁體中文（台灣）", "Hant", include_str!("../locales/zh-hant.json");
    Ja, "ja", "日本語", "Jpan", include_str!("../locales/ja.json");
    PtBr, "pt-br", "Português (Brasil)", "Latn", include_str!("../locales/pt-br.json");
}

// The settings file stores the BCP-47 code (`"zh-hans"`), never the Rust variant name, so a
// language can be renamed in code without invalidating anyone's settings.
impl Serialize for Locale {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for Locale {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        Locale::parse(&code).or_else(|| Locale::parse_tag(&code)).ok_or_else(|| serde::de::Error::custom(format!("unknown language {code:?}")))
    }
}

thread_local! {
    static LOCALE: std::cell::Cell<Locale> = const { std::cell::Cell::new(Locale::En) };
}

/// The UI language from the environment (`LIGHTCRAFT_LANGUAGE=zh-hans`), for headless runs.
pub fn default_language() -> Locale {
    std::env::var("LIGHTCRAFT_LANGUAGE").ok().and_then(|value| Locale::parse_tag(&value)).unwrap_or(Locale::En)
}

pub fn set_language(language: Locale) {
    LOCALE.with(|value| value.set(language));
}

pub fn language() -> Locale {
    LOCALE.with(std::cell::Cell::get)
}

/// Translate a built-in display label, preserving unknown labels verbatim.
/// Never call this on editable user text, filenames or command identifiers.
pub fn tr(source: &str) -> &str {
    tr_in(language(), source)
}

/// Catalog values are `'static` (they live in the embedded file), so a translation can be handed
/// out for as long as the `'static` key it was looked up by.
fn tr_in(language: Locale, source: &str) -> &str {
    if language.catalog().is_empty() {
        return source;
    }
    thread_local! {
        static VERBATIM: RefCell<BTreeMap<Locale, BTreeMap<String, &'static str>>> = const { RefCell::new(BTreeMap::new()) };
    }
    VERBATIM.with_borrow_mut(|cache| {
        let verbatim = cache.entry(language).or_insert_with(|| catalog_verbatim(language));
        verbatim.get(source).copied().unwrap_or(source)
    })
}

/// One language's catalog keyed for lookup, with values that outlive the catalog's own borrow.
fn catalog_verbatim(language: Locale) -> BTreeMap<String, &'static str> {
    language.catalog().iter().map(|(key, value)| (key.clone(), value.as_str())).collect()
}

// `tr_format!` (one arm per message, one `format!` per language) is generated by `build.rs` from
// `locales/*-formats.json`.
include!(concat!(env!("OUT_DIR"), "/tr-formats.rs"));

/// A stock preset's or profile's name in the UI language; an imported or user-edited name stays
/// exactly as its owner wrote it.
pub fn builtin_label(source: &str, builtin: bool) -> &str {
    if builtin { tr(source) } else { source }
}

/// An Activity (history) step for display. The step's stored label stays English; generated steps
/// are translated around the names they carry, and a user preset's name is kept verbatim.
pub fn history_label(source: &str, presets: &[lightcraft_develop::Preset]) -> String {
    if let Some(name) = source.strip_prefix("Preset: ") {
        // A user preset may share a stock preset's name: then the step could be either, so it
        // keeps the name as written.
        let builtin = presets.iter().any(|p| p.builtin && p.name == name) && !presets.iter().any(|p| !p.builtin && p.name == name);
        return tr_format!("Preset: {name}", name = builtin_label(name, builtin));
    }
    if let Some(name) = source.strip_prefix("Reset ") {
        return tr_format!("Reset {name}", name = tr(name));
    }
    if let Some(name) = source.strip_prefix("Quick Develop: ") {
        return tr_format!("Quick Develop: {name}", name = tr(name));
    }
    tr(source).to_string()
}

/// A library source's heading (All Photos, Recently Deleted…) in the UI language; an album's
/// name is the user's and stays verbatim.
pub fn source_label(source: lightcraft_engine::LibrarySource, catalog: &lightcraft_catalog::Catalog) -> String {
    let label = source.label(catalog);
    if matches!(source, lightcraft_engine::LibrarySource::Album(id) if catalog.album(id).is_some()) { label } else { tr(&label).to_string() }
}

/// A date group heading (`2026-09-20`, `2026-09`, `2026`) in the UI language: the grid's full
/// form, or the date sidebar's `short` one ("Sunday, 20" / "September" / "2026"). English keeps the
/// catalog's own wording; every other language formats it with its `*-formats.json` date
/// patterns and weekday names. The grouping keys themselves never change.
pub fn date_group_label(key: &str, short: bool) -> String {
    let label = lightcraft_catalog::dates::group_label(key);
    if language() == Locale::En {
        return if short {
            match key.len() {
                // "September 2026" → "September"
                7 => label.split(' ').next().unwrap_or(&label).to_string(),
                // "Sunday, 20 September 2026" → "Sunday, 20"
                10 => label.rsplitn(3, ' ').nth(2).unwrap_or(&label).to_string(),
                _ => label,
            }
        } else {
            label
        };
    }
    let Some(year) = key.get(..4).and_then(|y| y.parse::<u32>().ok()) else { return tr(&label).to_string() };
    if key.len() == 4 {
        return tr_format!("{year}", year = year);
    }
    let Some(month) = key.get(5..7).and_then(|m| m.parse::<u32>().ok()).filter(|m| (1..=12).contains(m)) else {
        return tr(&label).to_string();
    };
    if key.len() == 7 {
        return if short { tr_format!("{month}", month = month) } else { tr_format!("{month} {year}", month = month, year = year) };
    }
    if key.len() == 10
        && let Some(weekday) = lightcraft_catalog::dates::weekday(key)
        && let Some(day) = key.get(8..10).and_then(|d| d.parse::<u32>().ok())
    {
        let weekday = tr(weekday);
        return if short {
            tr_format!("{weekday}, {day}", weekday = weekday, day = day)
        } else {
            tr_format!("{weekday}, {day} {month} {year}", weekday = weekday, day = day, month = month, year = year)
        };
    }
    tr(&label).to_string()
}

/// A capture time for display in the UI language (English: "March 30, 2022 at 10:11:11 PM");
/// the metadata itself is never rewritten, and a value that doesn't parse is shown as it is.
pub fn display_time(iso: &str) -> String {
    if language() == Locale::En || lightcraft_catalog::dates::normalize_iso(iso).is_none() {
        return lightcraft_catalog::dates::display_time(iso);
    }
    let date = date_group_label(iso.get(..10).unwrap_or(iso), false);
    match iso.get(11..).filter(|time| !time.is_empty()) {
        Some(time) => format!("{date} {time}"),
        None => date,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_has_a_catalog_covering_the_source() {
        assert!(crate::i18n::Locale::En.catalog().is_empty(), "the source language needs no catalog");
        for language in Locale::ALL {
            assert!(!language.name().is_empty(), "{language:?} has a name");
            assert!(!language.script().is_empty(), "{language:?} has a script");
            assert_eq!(Locale::parse(language.code()), Some(*language), "{language:?} parses its own code");
            if *language == Locale::En {
                continue;
            }
            let messages = language.catalog();
            assert!(!messages.is_empty(), "{language:?}: {} has no messages", language.code());
            for (key, value) in messages {
                assert!(!value.is_empty(), "{language:?}: empty translation for {key:?}");
            }
        }
    }

    /// Where two catalogs translate the same text they agree on its placeholders. Catalogs may cover
    /// different sets of messages (a message a language lacks shows in English): the gaps are
    /// reported, not failed, so a translation can grow at its own pace.
    #[test]
    fn catalogs_agree_on_placeholders_and_report_gaps() {
        let fields = |text: &str| {
            let mut out: Vec<String> = Vec::new();
            let mut rest = text;
            while let Some(start) = rest.find('{') {
                let Some(end) = rest[start..].find('}') else { break };
                out.push(rest[start + 1..start + end].split(':').next().unwrap_or("").to_string());
                rest = &rest[start + end + 1..];
            }
            out.sort();
            out
        };
        let translated: Vec<Locale> = Locale::ALL.iter().copied().filter(|language| *language != Locale::En).collect();
        let all: std::collections::BTreeSet<&String> = translated.iter().flat_map(|language| language.catalog().keys()).collect();
        for language in &translated {
            let catalog = language.catalog();
            for (key, value) in catalog {
                assert_eq!(fields(key), fields(value), "{language:?}: {key:?} -> {value:?}");
            }
            let missing: Vec<&&String> = all.iter().filter(|key| !catalog.contains_key(key.as_str())).collect();
            if !missing.is_empty() {
                eprintln!("{} lacks {} message(s) another language translates (shown in English): {missing:?}", language.code(), missing.len());
            }
        }
    }

    /// Locale tags are matched leniently, so a system locale reaches a shipped language.
    #[test]
    fn locale_tags_normalize() {
        assert_eq!(Locale::parse_tag("en"), Some(Locale::En));
        assert_eq!(Locale::parse_tag("en-US"), Some(Locale::En));
        assert_eq!(Locale::parse_tag("zh-hans"), Some(Locale::ZhHans));
        assert_eq!(Locale::parse_tag("zh"), Some(Locale::ZhHans));
        assert_eq!(Locale::parse_tag("zh_CN"), Some(Locale::ZhHans));
        assert_eq!(Locale::parse_tag("zh-hant"), Some(Locale::ZhHant));
        assert_eq!(Locale::parse_tag("zh-Hant"), Some(Locale::ZhHant));
        assert_eq!(Locale::parse_tag("zh-TW"), Some(Locale::ZhHant));
        assert_eq!(Locale::parse_tag("zh_TW.UTF-8"), Some(Locale::ZhHant));
        assert_eq!(Locale::parse_tag("zh-HK"), Some(Locale::ZhHant));
        assert_eq!(Locale::parse_tag("zh-Hant-TW"), Some(Locale::ZhHant));
        assert_eq!(Locale::parse_tag("zh-CN"), Some(Locale::ZhHans));
        assert_eq!(Locale::parse_tag("ja_JP.UTF-8"), Some(Locale::Ja));
        assert_eq!(Locale::parse_tag("pt-br"), Some(Locale::PtBr));
        assert_eq!(Locale::parse_tag("pt-BR"), Some(Locale::PtBr));
        assert_eq!(Locale::parse_tag("pt_BR.UTF-8"), Some(Locale::PtBr));
        assert_eq!(Locale::parse_tag("pt"), Some(Locale::PtBr));
        assert_eq!(Locale::parse_tag("de"), None);
    }

    #[test]
    fn switching_language_translates_and_restores() {
        set_language(Locale::ZhHans);
        assert_eq!(tr("Exposure"), "曝光");
        assert_eq!(tr("Settings"), "设置");
        // Untranslated and data-like text passes through untouched.
        assert_eq!(tr("my-photo.jpg"), "my-photo.jpg");
        assert_eq!(tr("develop.set"), "develop.set");
        assert_eq!(tr("A string no catalog has"), "A string no catalog has");
        set_language(Locale::Ja);
        assert_eq!(tr("Exposure"), "露出");
        set_language(Locale::PtBr);
        assert_eq!(tr("Exposure"), "Exposição");
        assert_eq!(tr("Settings"), "Configurações");
        set_language(Locale::En);
        assert_eq!(tr("Exposure"), "Exposure");
    }

    /// Format strings carry their values in every language, and a language without a translation
    /// falls back to the English format rather than dropping the values.
    #[test]
    fn formats_render_in_every_language() {
        // Every language's format string is checked by `format!` against the call site's values.
        set_language(Locale::En);
        assert_eq!(tr_format!("Imported {} photo{}", 12, "s"), "Imported 12 photos");
        assert_eq!(tr_format!("Exported {ok} of {total} photo{}", "s", ok = 4, total = 12), "Exported 4 of 12 photos");
        set_language(Locale::ZhHans);
        assert_eq!(tr_format!("Imported {} photo{}", 12, "s"), "已导入 12 张照片");
        assert_eq!(tr_format!("Exported {ok} of {total} photo{}", "s", ok = 4, total = 12), "已导出 12 张中的 4 张照片");
        set_language(Locale::Ja);
        assert_eq!(tr_format!("Imported {} photo{}", 12, "s"), "12枚を読み込みました");
        assert_eq!(tr_format!("Exported {ok} of {total} photo{}", "s", ok = 4, total = 12), "12枚中4枚を書き出しました");
        // Format specs survive translation: precision, sign, and values a language reorders.
        for language in Locale::ALL {
            set_language(*language);
            let text = tr_format!("{count} smart preview{} · {:.1} MB", "s", 12.345, count = 3);
            assert!(text.contains("12.3") && !text.contains("12.34"), "{language:?}: {text}");
            let merging = tr_format!("Merging… {stage} {:.0}%", 42.4, stage = "Aligning");
            assert!(merging.contains("42%"), "{language:?}: {merging}");
            let delta = tr_format!("{label} {d:+} on every selected photo", label = "Exposure", d = 5);
            assert!(delta.contains("+5"), "{language:?}: {delta}");
            let reading = tr_format!("Reading photos… {} of {}", 3, 10);
            let (three, ten) = (reading.find('3'), reading.find("10"));
            assert!(three.is_some() && ten.is_some(), "{language:?}: {reading}");
        }
        set_language(Locale::En);
        assert_eq!(tr_format!("Reading photos… {} of {}", 3, 10), "Reading photos… 3 of 10");
    }

    /// Every character a catalog paints has a glyph in the fonts the UI installs, so a translation
    /// never shows as boxes. Without craft-fonts there are no CJK faces at all, and the CJK
    /// catalogs are skipped (the same degradation the Japanese UI has always had).
    #[test]
    fn catalog_characters_are_paintable() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions(lightcraft_engine::CRAFT_FONTS));
        let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
        out.textures_delta.clear();
        for language in Locale::ALL {
            let messages = language.catalog();
            if messages.is_empty() {
                continue;
            }
            // The faces this language's script needs; a language the craft-fonts input does not
            // cover (a translation added ahead of its font) is reported, not failed.
            let mut faces: Vec<&str> =
                lightcraft_engine::CRAFT_FONTS.iter().filter(|font| font.serves(language.script())).map(|font| font.family).collect();
            faces.dedup();
            if faces.is_empty() {
                eprintln!("skipped {}: built without a craft-fonts face for {}", language.code(), language.script());
                continue;
            }
            ctx.fonts_mut(|fonts| {
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                    let font = egui::FontId::new(13.0, family.clone());
                    let mut missing: Vec<char> = messages
                        .values()
                        .flat_map(|message| message.chars())
                        .filter(|ch| !ch.is_whitespace() && !fonts.has_glyph(&font, *ch))
                        .collect();
                    missing.sort_unstable();
                    missing.dedup();
                    assert!(
                        missing.is_empty(),
                        "{} in {family:?} (faces: {faces:?}) lack glyphs: {}",
                        language.code(),
                        missing.iter().collect::<String>()
                    );
                }
            });
        }
    }

    /// The interface's own symbols paint in every language: the CJK faces stop at Han and kana, so a
    /// translation must not carry a symbol no installed font has (egui's defaults cover the rest).
    #[test]
    fn interface_symbols_are_paintable() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions(lightcraft_engine::CRAFT_FONTS));
        let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
        out.textures_delta.clear();
        // ⌥ and ⌫ come from egui's bundled fonts; ▸ and ▾ are in neither (measured 2026-10-07).
        ctx.fonts_mut(|fonts| {
            for symbol in ['⌥', '⌫', '⇧', '⌘', '→', '…', '—', '“', '”', '·', '»', '⌄', '★'] {
                let font = egui::FontId::new(13.0, egui::FontFamily::Proportional);
                assert!(fonts.has_glyph(&font, symbol), "{symbol:?} has no glyph in any installed font");
            }
        });
    }

    /// The language menu covers every language, and a language's own command selects it.
    #[test]
    fn language_commands_cover_every_language() {
        let commands = [
            ("app.language.traditionalChinese", Locale::ZhHant),
            ("app.language.english", Locale::En),
            ("app.language.simplifiedChinese", Locale::ZhHans),
            ("app.language.japanese", Locale::Ja),
            ("app.language.portuguese", Locale::PtBr),
        ];
        // One command per language, and every command reachable from the menu table.
        assert_eq!(commands.len(), Locale::ALL.len());
        for (id, language) in commands {
            assert_eq!(crate::menus::language_from_command(id), Some(language), "{id}");
            assert!(crate::menus::ui_commands().any(|command| command.0 == id), "{id} is not in the menu");
        }
        assert_eq!(crate::menus::language_from_command("app.language.klingon"), None);
        assert_eq!(crate::menus::language_from_command("view.detail"), None);
    }

    #[test]
    fn preferences_round_trip_and_old_settings_remain_readable() {
        let old: crate::state::UiState = serde_json::from_str("{}").unwrap();
        assert_eq!(old.language, Locale::En);
        // Settings written before the language list grew still load.
        let legacy: crate::state::UiState = serde_json::from_str(r#"{"language":"ja"}"#).unwrap();
        assert_eq!(legacy.language, Locale::Ja);
        let settings = crate::state::UiState { language: Locale::ZhHans, ..old };
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.language, Locale::ZhHans);
    }

    #[test]
    fn catalogs_contain_core_workflows() {
        for language in Locale::ALL.iter().filter(|language| **language != Locale::En) {
            for key in ["Import Photos…", "Export…", "Exposure", "White Balance", "Settings", "Language"] {
                let value = language.tr(key);
                assert!(!value.is_empty() && value != key, "{language:?}: {key}");
            }
        }
    }

    /// User-named menu items (presets, albums, label sets) are never translated; built-in labels are.
    #[test]
    fn menu_labels_translate_but_user_names_survive() {
        for language in Locale::ALL {
            set_language(*language);
            assert_eq!(crate::menubar::display_item_label("album.addPhotos", &serde_json::json!({"id": 1}), "Color"), "Color");
            assert_eq!(crate::menubar::display_item_label("app.export", &serde_json::json!({"preset": "Color"}), "Color"), "Color");
            assert_eq!(crate::menubar::display_item_label("view.photoGrid", &serde_json::Value::Null, "Color"), language.tr("Color"));
        }
        set_language(Locale::ZhHans);
        assert_eq!(crate::menubar::display_item_label("view.photoGrid", &serde_json::Value::Null, "Color"), "颜色");
        set_language(Locale::Ja);
        assert_eq!(crate::menubar::display_item_label("view.photoGrid", &serde_json::Value::Null, "Color"), "カラー");
        set_language(Locale::En);
    }

    /// The text the whole window paints over a few frames in `language`.
    fn painted_text(ctx: &egui::Context, app: &mut crate::LightcraftApp, language: Locale) -> String {
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
        app.ui.language = language;
        let mut text = String::new();
        for frame in 0..4 {
            let input = crate::headless::HeadlessView::raw_input(egui::vec2(1600.0, 1000.0), 1.0, frame as f64 / 60.0, vec![]);
            let mut out = ctx.run_ui(input, |ui| {
                app.logic(ui.ctx());
                app.ui(ui);
            });
            // This inspects shapes without a renderer; discard texture uploads explicitly.
            out.textures_delta.clear();
            text.clear();
            for shape in out.shapes {
                collect(&shape.shape, &mut text);
            }
        }
        text
    }

    /// Every language is painted by the real window, switching at runtime reinstalls the fonts
    /// (their CJK fallback order follows the language), and command ids never change with it.
    #[test]
    fn every_language_is_painted_and_menu_ids_stay_the_same() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        app.ui.left_panel = true;
        let ids = |app: &crate::LightcraftApp| crate::menus::menu_entries(app).into_iter().map(|entry| entry.id).collect::<Vec<_>>();
        let english_ids = ids(&app);
        for language in Locale::ALL {
            let text = painted_text(&ctx, &mut app, *language);
            for label in ["My Photos", "All Photos"] {
                assert!(text.contains(language.tr(label)), "{language:?}: {label} -> {:?} not in\n{text}", language.tr(label));
            }
            assert_eq!(app.font_language, *language, "the fonts follow the language");
            assert_eq!(ids(&app), english_ids, "{language:?}: command ids are presentation-independent");
        }
        let text = painted_text(&ctx, &mut app, Locale::Ja);
        assert!(text.contains("マイフォト") && text.contains("すべての写真"), "{text}");
        let text = painted_text(&ctx, &mut app, Locale::ZhHans);
        assert!(text.contains("我的照片") && text.contains("所有照片"), "{text}");
        let text = painted_text(&ctx, &mut app, Locale::PtBr);
        assert!(text.contains("Minhas fotos") && text.contains("Todas as fotos"), "{text}");
        set_language(Locale::En);
    }

    /// The UI families, after one frame so the font definitions are loaded.
    fn fonts_ctx(craft: &'static [lightcraft_engine::CraftFont]) -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions(craft));
        let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
        out.textures_delta.clear();
        ctx
    }

    /// Built with craft-fonts, every CJK language renders with real glyphs (no tofu) in every UI
    /// family, Monospace included.
    #[test]
    fn craft_fonts_render_cjk_in_the_ui() {
        let samples = [(Locale::Ja, "日本語の文字"), (Locale::ZhHans, "简体中文字"), (Locale::ZhHant, "繁體中文字")];
        for (language, sample) in samples {
            if !lightcraft_engine::CRAFT_FONTS.iter().any(|font| font.serves(language.script())) {
                eprintln!("skipped {}: built without a craft-fonts face for {}", language.code(), language.script());
                continue;
            }
            set_language(language);
            let ctx = fonts_ctx(lightcraft_engine::CRAFT_FONTS);
            ctx.fonts_mut(|fonts| {
                for family in
                    [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into()), egui::FontFamily::Monospace]
                {
                    let font = egui::FontId::new(13.0, family);
                    for ch in sample.chars() {
                        assert!(fonts.has_glyph(&font, ch), "{language:?}: {ch} in {font:?}");
                    }
                    let galley = fonts.layout_no_wrap(sample.into(), font.clone(), egui::Color32::WHITE);
                    let wide = 13.0 * (sample.chars().count() as f32 - 1.0);
                    assert!(galley.size().x > wide, "{language:?} {font:?}: full-width glyphs, {:?}", galley.size());
                }
            });
        }
        set_language(Locale::En);
    }

    /// Built without craft-fonts, the UI (in every language) still installs its fonts and runs;
    /// Latin text keeps Inter.
    #[test]
    fn the_ui_works_without_craft_fonts() {
        let ctx = fonts_ctx(&[]);
        ctx.fonts_mut(|fonts| {
            // (Not Monospace: egui's `has_glyph` reports false for glyphs of the family's
            // replacement-glyph face, which there is Hack, the face that draws Latin.)
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                let font = egui::FontId::new(13.0, family);
                assert!("LightCraft".chars().all(|ch| fonts.has_glyph(&font, ch)), "{font:?}");
            }
        });
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        for language in Locale::ALL {
            app.ui.language = *language;
            for frame in 0..3 {
                let input = crate::headless::HeadlessView::raw_input(egui::vec2(1200.0, 800.0), 1.0, frame as f64 / 60.0, vec![]);
                let mut out = ctx.run_ui(input, |ui| {
                    app.logic(ui.ctx());
                    app.ui(ui);
                });
                out.textures_delta.clear();
            }
        }
        set_language(Locale::En);
    }

    /// Traditional Chinese through the menu command and the control channel, persisted, with
    /// localised headings and dates; ids and user text are untouched.
    #[test]
    fn traditional_chinese_switches_through_menu_and_control_and_persists() {
        use crate::control::{ControlRequest, Outcome};
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        app.run("app.language.traditionalChinese", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Locale::ZhHant);
        // Applied at once, not on the next frame.
        assert_eq!(tr("File"), "檔案");
        assert_eq!(source_label(lightcraft_engine::LibrarySource::All, &app.session.catalog), "所有照片");
        assert_eq!(date_group_label("2026-09-20", false), "2026年9月20日 星期日");
        assert_eq!(date_group_label("2026-09-20", true), "20日 星期日");
        assert_eq!(date_group_label("2026-09", false), "2026年9月");
        assert_eq!(date_group_label("2026-09", true), "9月");
        assert_eq!(date_group_label("2026", false), "2026年");
        assert_eq!(date_group_label("", false), "未知日期");
        assert_eq!(display_time("2026-09-20T16:04:05"), "2026年9月20日 星期日 16:04:05");
        assert_eq!(display_time("a-user-value"), "a-user-value");
        assert_eq!(crate::menubar::checked(&app, "app.language.traditionalChinese"), Some(true));
        assert_eq!(crate::menubar::checked(&app, "app.language.english"), Some(false));
        assert_eq!(tr("my-photo.jpg"), "my-photo.jpg");
        for id in ["album.addPhotos", "metadata.applyPreset", "label.applySet"] {
            assert_eq!(crate::menubar::display_item_label(id, &serde_json::json!({"id": 1}), "Color"), "Color");
        }
        assert_eq!(crate::menubar::display_item_label("view.photoGrid", &serde_json::Value::Null, "Color"), "色彩");
        assert_eq!(tr_format!("{n} photo{}", "s", n = 12), "12 張照片");
        assert_eq!(tr_format!("Exported {ok} of {total} photo{}", "s", ok = 4, total = 12), "已匯出 4／12 張照片");
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert!(saved.contains(r#""language":"zh-hant""#), "{saved}");
        assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, Locale::ZhHant);
        let ctx = egui::Context::default();
        app.ui.language = Locale::En;
        for code in ["zh-hant", "zh-Hant", "zh-TW", "zh_TW", "zh-HK"] {
            let (req, _) = ControlRequest::new("ui.set", serde_json::json!({"language": code}));
            let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &req) else { panic!("expected a reply") };
            assert_eq!(reply["ok"], true, "{code}");
            assert_eq!(app.ui.language, Locale::ZhHant, "{code}");
            assert_eq!(language(), Locale::ZhHant, "{code} applies at once");
        }
        let (req, _) = ControlRequest::new("ui.set", serde_json::json!({"language": "xx"}));
        let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &req) else { panic!("expected a reply") };
        assert_eq!(reply["ok"], false);
        assert_eq!(app.ui.language, Locale::ZhHant);
        let ids = |app: &crate::LightcraftApp| crate::menus::menu_entries(app).into_iter().map(|entry| entry.id).collect::<Vec<_>>();
        let chinese_ids = ids(&app);
        app.run("app.language.english", serde_json::json!({})).unwrap();
        assert_eq!(tr("File"), "File");
        assert_eq!(chinese_ids, ids(&app));
    }

    /// Dates follow every language's own patterns; English keeps the catalog's wording.
    #[test]
    fn date_headings_in_every_language() {
        set_language(Locale::En);
        assert_eq!(date_group_label("2026-09-20", false), "Sunday, 20 September 2026");
        assert_eq!(date_group_label("2026-09-20", true), "Sunday, 20");
        assert_eq!(date_group_label("2026-09", true), "September");
        assert_eq!(display_time("2026-09-20T16:04:05"), lightcraft_catalog::dates::display_time("2026-09-20T16:04:05"));
        set_language(Locale::Ja);
        assert_eq!(date_group_label("2026-09-20", false), "2026年9月20日（日曜日）");
        assert_eq!(date_group_label("2026-09", true), "9月");
        set_language(Locale::ZhHans);
        assert_eq!(date_group_label("2026-09-20", false), "2026年9月20日 星期日");
        for language in Locale::ALL {
            set_language(*language);
            // A key that is not a date never panics and never shows a raw pattern.
            for key in ["", "abcd", "2026-xx", "2026-13", "2026-09-xx", "２０２６-09"] {
                assert!(!date_group_label(key, false).contains('{'), "{language:?}: {key:?}");
                assert!(!date_group_label(key, true).contains('{'), "{language:?}: {key:?}");
            }
        }
        set_language(Locale::En);
    }

    /// Stock presets and panel headers are translated; a user preset keeps its name, also when it
    /// shares a stock preset's name.
    #[test]
    fn traditional_chinese_presets_and_panel_headers_are_painted() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        app.ui.presets = true;
        let builtin = app.session.presets.iter().find(|p| p.name == "Warm Glow").unwrap().clone();
        app.session.presets.push(lightcraft_develop::Preset { id: "user.test".into(), builtin: false, ..builtin });
        for (panel, title) in [(crate::state::RightPanel::Activity, "歷史紀錄"), (crate::state::RightPanel::Versions, "版本")] {
            app.ui.right = panel;
            let text = painted_text(&ctx, &mut app, Locale::ZhHant);
            assert!(text.contains(title), "{text}");
            assert!(!text.contains("History") && !text.contains("Versions"), "{text}");
            assert!(text.contains("暖光"), "stock preset: {text}");
            assert!(text.contains("Warm Glow"), "user preset: {text}");
            assert!(text.contains("色彩"), "stock group: {text}");
        }
        assert_eq!(history_label("Exposure", &app.session.presets), "曝光");
        assert_eq!(history_label("Preset: Warm Film", &app.session.presets), "預設集：暖調底片");
        assert_eq!(history_label("Preset: Warm Glow", &app.session.presets), "預設集：Warm Glow");
        set_language(Locale::En);
    }
}
