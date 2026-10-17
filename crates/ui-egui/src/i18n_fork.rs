//! The fork's own UI messages per language (`locales/fork/<code>.json`, fork-owned), merged over
//! upstream's catalog (`locales/<code>.json`) when it is parsed, so the shared catalogs carry only
//! upstream's rows and merge without conflicts. Formats: `build.rs` merges `locales/fork/<code>-formats.json`.

/// The overlay catalog of a language code, if the fork has one.
pub(crate) fn overlay(code: &str) -> Option<&'static str> {
    match code {
        "de" => Some(include_str!("../locales/fork/de.json")),
        "es" => Some(include_str!("../locales/fork/es.json")),
        "fr" => Some(include_str!("../locales/fork/fr.json")),
        "ja" => Some(include_str!("../locales/fork/ja.json")),
        "pt-br" => Some(include_str!("../locales/fork/pt-br.json")),
        "ru" => Some(include_str!("../locales/fork/ru.json")),
        "uk" => Some(include_str!("../locales/fork/uk.json")),
        "zh-hans" => Some(include_str!("../locales/fork/zh-hans.json")),
        "zh-hant" => Some(include_str!("../locales/fork/zh-hant.json")),
        // languages upstream added after the fork's overlays: so far only the words the smart-album
        // rule editor needs in every language (see `tests_i18n_coverage::PARTIAL`)
        "pl" => Some(include_str!("../locales/fork/pl.json")),
        "cs" => Some(include_str!("../locales/fork/cs.json")),
        "it" => Some(include_str!("../locales/fork/it.json")),
        "ka" => Some(include_str!("../locales/fork/ka.json")),
        _ => None,
    }
}
