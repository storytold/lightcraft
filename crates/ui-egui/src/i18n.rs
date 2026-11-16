//! Presentation-only localization. Command ids, user names and catalog data remain unchanged.
//!
//! A language is one entry in [`language_table!`] below: its BCP-47 code, the endonym shown in the
//! Language menus, the ISO 15924 script its text needs (which picks the CJK font fallback), and its
//! message catalog (`locales/<code>.json`, embedded at build time). Adding a language is that entry
//! plus its catalogs and the corresponding language command in `menus.rs`. Settings read the
//! locale table, while menus and the control channel share the command mapping.

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
                // English text, it never takes the app down. An empty translation is a missing one.
                let parse = |catalog: Option<&'static str>| match catalog {
                    Some(json) => match serde_json::from_str::<BTreeMap<String, String>>(json) {
                        // Keys stay as written (`{app}` and all) so source text finds them; values are
                        // shown, so the brand placeholders are filled in once, here.
                        Ok(messages) => messages.into_iter().filter(|(_, text)| !text.is_empty()).map(|(key, value)| (key, brand_fill(&value))).collect(),
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
                                let mut messages = parse(catalog);
                                messages.extend(parse(crate::i18n_fork::overlay(language.code())));
                                (*language, messages)
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
    Es, "es", "Español", "Latn", include_str!("../locales/es.json");
    De, "de", "Deutsch", "Latn", include_str!("../locales/de.json");
    Ru, "ru", "Русский", "Cyrl", include_str!("../locales/ru.json");
    Fr, "fr", "Français", "Latn", include_str!("../locales/fr.json");
    Uk, "uk", "Українська", "Cyrl", include_str!("../locales/uk.json");
    Pl, "pl", "Polski", "Latn", include_str!("../locales/pl.json");
    Cs, "cs", "Čeština", "Latn", include_str!("../locales/cs.json");
    It, "it", "Italiano", "Latn", include_str!("../locales/it.json");
    Ka, "ka", "ქართული", "Geor", include_str!("../locales/ka.json");
    Sv, "sv", "Svenska", "Latn", include_str!("../locales/sv.json");
    Fi, "fi", "Suomi", "Latn", include_str!("../locales/fi.json");
    Id, "id", "Bahasa Indonesia", "Latn", include_str!("../locales/id.json");
    Vi, "vi", "Tiếng Việt", "Latn", include_str!("../locales/vi.json");
}

impl Locale {
    /// Every language in the order the Settings row and Edit ▸ Language list them: English first
    /// (the source language), then the rest by their own name with accents folded, so Čeština
    /// sits under C. Names in other scripts follow the Latin ones in code point order: Cyrillic,
    /// Georgian, then the CJK languages.
    pub fn for_picker() -> Vec<Self> {
        let mut rest: Vec<Self> = Self::ALL.iter().copied().filter(|l| *l != Self::En).collect();
        rest.sort_by_cached_key(|l| name_sort_key(l.name()));
        std::iter::once(Self::En).chain(rest).collect()
    }
}

/// `name` lowercased with the Latin accents that occur in language names folded away, so a
/// byte-wise sort reads alphabetically. Other scripts are left as they are.
fn name_sort_key(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'č' => 'c',
            'è'..='ë' | 'ē' | 'ě' | 'ę' | 'ế' | 'ệ' => 'e',
            'ì'..='ï' | 'ī' => 'i',
            'ñ' | 'ń' | 'ň' => 'n',
            'ò'..='ö' | 'ø' | 'ő' => 'o',
            'ś' | 'š' => 's',
            'ù'..='ü' | 'ū' | 'ů' | 'ű' => 'u',
            'ý' | 'ÿ' => 'y',
            'ź' | 'ż' | 'ž' => 'z',
            c => c,
        })
        .collect()
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
    /// The host's answer for the "Auto" language ([`set_default_language`]).
    static DEFAULT_LANGUAGE: std::cell::Cell<Option<Locale>> = const { std::cell::Cell::new(None) };
}

/// Install the language the "Auto" setting resolves to when nothing is stored. The browser host
/// calls this once at startup with the browser's own languages ([`locale_from_tags`]); the
/// desktop host doesn't: there Auto keeps following the environment. Must run before the UI state
/// is first constructed.
pub fn set_default_language(language: Locale) {
    DEFAULT_LANGUAGE.with(|slot| slot.set(Some(language)));
}

/// The first of `tags` (BCP-47, most preferred first, as `navigator.languages` lists them) that
/// names a shipped language; None when none does.
pub fn locale_from_tags(tags: &[String]) -> Option<Locale> {
    tags.iter().find_map(|tag| Locale::parse_tag(tag))
}

/// The UI language "Auto" resolves to: the environment's (`<ENV_PREFIX>_LANGUAGE=zh-hans`, for
/// headless runs), or the host-installed default ([`set_default_language`], the browser's languages
/// on the web), else English.
pub fn default_language() -> Locale {
    lcc_brand::env("LANGUAGE")
        .and_then(|value| Locale::parse_tag(&value))
        .or_else(|| DEFAULT_LANGUAGE.with(std::cell::Cell::get))
        .unwrap_or(Locale::En)
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

/// Every translation in `language`'s catalog (a painted text that is one is translated, even when
/// it is also some English message: German "Profile" for "Profiles").
#[cfg(test)]
pub(crate) fn translations(language: Locale) -> std::collections::BTreeSet<&'static str> {
    language.catalog().values().map(String::as_str).collect()
}

/// The messages whose translation depends on where they appear, as (context, English message): one
/// English word with two meanings in the UI (`Tracking`: the book's letter spacing and the camera's
/// AF area). Such a place asks for `"<context>|<English>"`, through [`tr`] or any helper that
/// translates; a catalog entry keyed that way wins there, else the plain message is shown. A language
/// adds the contextual entry only where its plain translation reads wrongly in that place, so every
/// language carries the plain message and only some carry a contextual one.
pub(crate) const CONTEXTUAL: &[(&str, &str)] = &[
    (BOOL_CONTEXT, "Yes"),
    (BOOL_CONTEXT, "No"),
    ("book text", "Tracking"),
    ("focus", "Tracking"),
    ("book graphic", "Frame"),
    ("time-lapse", "Frame"),
    ("color wheels", "Log"),
    ("plug-in tab", "Log"),
];

/// `"<context>|<English>"` split into its context and English message, when it is one of
/// [`CONTEXTUAL`]'s (other text with a `|` in it is a plain message).
pub(crate) fn contextual(source: &str) -> Option<(&str, &str)> {
    source.split_once('|').filter(|pair| CONTEXTUAL.contains(pair))
}

/// [`tr`] for a message whose translation depends on where it appears ([`CONTEXTUAL`]): a catalog
/// entry keyed `"<context>|<source>"` wins, else the plain message.
fn tr_ctx_in<'a>(language: Locale, context: &str, source: &'a str) -> &'a str {
    match language.catalog().get(&format!("{context}|{source}")) {
        Some(translation) => translation.as_str(),
        None => tr_in(language, source),
    }
}

/// Catalog values are `'static` (they live in the embedded file), so a translation can be handed
/// out for as long as the `'static` key it was looked up by.
fn tr_in(language: Locale, source: &str) -> &str {
    if let Some((context, plain)) = contextual(source) {
        return tr_ctx_in(language, context, plain);
    }
    if language.catalog().is_empty() {
        return filled_source(source);
    }
    thread_local! {
        static VERBATIM: RefCell<BTreeMap<Locale, BTreeMap<String, &'static str>>> = const { RefCell::new(BTreeMap::new()) };
    }
    VERBATIM.with_borrow_mut(|cache| {
        let verbatim = cache.entry(language).or_insert_with(|| catalog_verbatim(language));
        verbatim.get(source).copied().unwrap_or_else(|| filled_source(source))
    })
}

/// Fills the brand placeholders of catalog and source text: `{app}` (the display name, via
/// [`lcc_brand::fill`], which also fills `{upstream}`), `{cli}` (the command-line binary) and `{env}` (the
/// environment variable prefix).
pub fn brand_fill(text: &str) -> String {
    if !text.contains('{') {
        return text.to_string();
    }
    lcc_brand::fill(text).replace("{cli}", lcc_brand::CLI_BINARY).replace("{env}", lcc_brand::ENV_PREFIX)
}

/// Source text with its brand placeholders filled in. Text without a placeholder is returned as is;
/// filled text is interned (the set of built-in messages is bounded), so it can be handed out like
/// the source.
fn filled_source(source: &str) -> &str {
    if !["{app}", "{cli}", "{env}", "{upstream}"].iter().any(|placeholder| source.contains(placeholder)) {
        return source;
    }
    thread_local! {
        static FILLED: RefCell<BTreeMap<String, &'static str>> = const { RefCell::new(BTreeMap::new()) };
    }
    FILLED.with_borrow_mut(|cache| *cache.entry(source.to_string()).or_insert_with(|| Box::leak(brand_fill(source).into_boxed_str())))
}

/// One language's catalog keyed for lookup, with values that outlive the catalog's own borrow.
fn catalog_verbatim(language: Locale) -> BTreeMap<String, &'static str> {
    language.catalog().iter().map(|(key, value)| (key.clone(), value.as_str())).collect()
}

// `tr_format!` (one arm per message, one `format!` per language) is generated by `build.rs` from
// `locales/*-formats.json`.
include!(concat!(env!("OUT_DIR"), "/tr-formats.rs"));

/// The web build's notice for a dropped file the browser could not read.
pub fn dropped_file_unreadable(name: &str, error: &str) -> String {
    tr_format!("Could not read {name}: {e}", name = name, e = error)
}

/// The web build's notice for dropped files the library refused.
pub fn dropped_import_failed(error: &str) -> String {
    tr_format!("Import failed: {e}", e = error)
}

/// The web build's notice for `n` dropped photos that joined the library.
pub fn dropped_photos_added(n: usize) -> String {
    tr_format!("Added {n} photo{}", if n == 1 { "" } else { "s" }, n = n)
}

/// The web build's notice for `n` dropped photos that came back from Recently Deleted.
pub fn dropped_photos_restored(n: usize) -> String {
    tr_format!("Restored {} photo{} from Recently Deleted", n, if n == 1 { "" } else { "s" })
}

/// The web build's notice for what a drop of photos came to, from the `library.import` report: every outcome that
/// happened (added, restored from Recently Deleted, not readable), joined with " · " as the desktop import's notice
/// joins its parts, or `None` when none did (duplicates only).
pub fn dropped_photos_outcome(report: &serde_json::Value) -> Option<String> {
    let count = |k: &str| report[k].as_array().map_or(0, Vec::len);
    let parts: Vec<String> = [
        (count("imported"), dropped_photos_added as fn(usize) -> String),
        (count("restored"), dropped_photos_restored),
        (count("failed"), dropped_photos_unreadable),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, say)| say(n))
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The web build's notice for `n` dropped photos that could not be read.
pub fn dropped_photos_unreadable(n: usize) -> String {
    tr_format!("{} photo{} not readable", n, if n == 1 { "" } else { "s" })
}

/// The title and text of the message box shown when the desktop app's window can't start
/// (issue #260), in the UI language: the error and, when there is one, the log file to look in.
pub fn startup_failed_message(error: &str, log_file: Option<&str>) -> (String, String) {
    let text = match log_file {
        Some(path) => tr_format!("{app} could not open its window: {e}\n\nThe log file has the details: {path}", e = error, path = path),
        None => tr_format!("{app} could not open its window: {e}", e = error),
    };
    (tr("{app} could not start").to_string(), text)
}

/// The title and text of the message box shown when the app stopped while its window was up
/// (issue #1337: the window's renderer after the graphics driver reset the GPU), in the UI
/// language: what happened and, when there is one, the log file to look in.
pub fn stopped_message(error: &str, log_file: Option<&str>) -> (String, String) {
    let text = match log_file {
        Some(path) => tr_format!("{app} stopped unexpectedly: {e}\n\nThe log file has the details: {path}", e = error, path = path),
        None => tr_format!("{app} stopped unexpectedly: {e}", e = error),
    };
    (tr("{app} stopped").to_string(), text)
}

/// Whether sentence case may upper-case `c`. Georgian has no case: upper-casing Mkhedruli would
/// turn it into Mtavruli, a different (all-caps) script, so Georgian letters stay as written.
pub fn capitalizes(c: char) -> bool {
    !matches!(c, '\u{10A0}'..='\u{10FF}' | '\u{1C90}'..='\u{1CBF}' | '\u{2D00}'..='\u{2D2F}')
}

/// `text` with its first letter upper-cased ("its input" → "Its input"), unless [`capitalizes`]
/// says that letter has no capital to take.
pub fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if capitalizes(first) => first.to_uppercase().chain(chars).collect(),
        _ => text.to_string(),
    }
}

/// A stock preset's or profile's name in the UI language; an imported or user-edited name stays
/// exactly as its owner wrote it.
pub fn builtin_label(source: &str, builtin: bool) -> &str {
    if builtin { tr(source) } else { source }
}

/// A profile's name for display: a built-in profile's is translated, an imported LUT's is shown as
/// its file named it (it may happen to match a message, like "Vivid").
pub fn profile_label<'a>(id: &str, name: &'a str) -> &'a str {
    builtin_label(name, lcc_engine::presets::profile(id).is_some())
}

/// An Activity (history) step for display. The step's stored label stays English; generated steps
/// are translated around the names they carry, and a user preset's name is kept verbatim.
pub fn history_label(source: &str, presets: &[lcc_develop::Preset]) -> String {
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
pub fn source_label(source: lcc_engine::LibrarySource, catalog: &lcc_catalog::Catalog) -> String {
    // (Classic: the Catalog panel's name for it, as Lightroom Classic's)
    let label = if source == lcc_engine::LibrarySource::All { "All Photographs".into() } else { source.label(catalog) };
    if matches!(source, lcc_engine::LibrarySource::Album(id) if catalog.album(id).is_some()) { label } else { tr(&label).to_string() }
}

/// What the grid is titled for the session's source: [`source_label`], or a library folder's last
/// two names (`photos/travel`; the folder's name is the user's and stays verbatim).
pub fn source_title(session: &lcc_engine::Session) -> String {
    match (session.source, session.library_folder.as_deref()) {
        (lcc_engine::LibrarySource::LibraryFolder, Some(path)) => lcc_catalog::folders::folder_label(path),
        (source, _) => source_label(source, &session.catalog),
    }
}

/// A date group heading (`2026-09-20`, `2026-09`, `2026`) in the UI language: the grid's full
/// form, or the date sidebar's `short` one ("Sunday, 20" / "September" / "2026"). English keeps the
/// catalog's own wording; every other language formats it with its `*-formats.json` date
/// patterns and weekday names. The grouping keys themselves never change.
pub fn date_group_label(key: &str, short: bool) -> String {
    let label = lcc_catalog::dates::group_label(key);
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
        && let Some(weekday) = lcc_catalog::dates::weekday(key)
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
/// Every language reads the same second precision English does: sub-second digits and a time-zone
/// suffix in the metadata are not shown (#743).
pub fn display_time(iso: &str) -> String {
    // a year or a month alone ("1983", "April 1983"), as the grid's date headers show it
    if lcc_catalog::dates::partial_capture(iso).is_some() {
        return date_group_label(iso, false);
    }
    // the normalized form carries only whole seconds, so every language shows what English shows;
    // a value that doesn't normalize falls back (shown as it is, as English's fallback does)
    let normalized = match lcc_catalog::dates::normalize_iso(iso) {
        Some(n) => n,
        None => return lcc_catalog::dates::display_time(iso),
    };
    if language() == Locale::En {
        return lcc_catalog::dates::display_time(iso);
    }
    let date = date_group_label(normalized.get(..10).unwrap_or(iso), false);
    match normalized.get(11..).filter(|time| !time.is_empty()) {
        Some(time) => format!("{date} {time}"),
        None => date,
    }
}

/// Built-in colour names are translated; custom label names are user data.
pub fn color_label(catalog: &lcc_catalog::Catalog, label: lcc_catalog::ColorLabel) -> String {
    catalog.custom_label_name(label).map_or_else(|| tr(&catalog.label_name(label)).to_string(), str::to_string)
}

/// A legacy smart-album filter summary, using the same display labels as filter chips.
pub fn filter_label(filter: &lcc_catalog::Filter, catalog: &lcc_catalog::Catalog) -> String {
    if language() == Locale::En {
        filter.describe_with(catalog)
    } else {
        lcc_engine::filter_chips(filter, catalog)
            .iter()
            .map(|chip| crate::panels::chips::display_label(chip, filter, catalog))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// What people read for a smart-album rule's problem, under its row: the field's name and the
/// issue, in the current language ("Title: needs something to look for").
pub fn problem_text(problem: &lcc_catalog::rules::Problem) -> String {
    problem_text_in(language(), problem)
}

/// [`problem_text`] with the rule's position first ("#2.1 Rating: …"), for lists away from the
/// editor (the sidebar's tooltip).
pub fn problem_line(problem: &lcc_catalog::rules::Problem) -> String {
    problem_line_in(language(), problem)
}

fn problem_text_in(language: Locale, problem: &lcc_catalog::rules::Problem) -> String {
    let issue = tr_in(language, problem.issue.text());
    // Japanese and Chinese catalogs join a label and its text with a full-width colon
    let colon = if matches!(language.script(), "Jpan" | "Hans" | "Hant") { "：" } else { ": " };
    match problem.field.as_deref() {
        Some(field) => format!("{}{colon}{issue}", tr_in(language, lcc_catalog::rules::field_label(field).unwrap_or(field))),
        None => issue.to_string(),
    }
}

fn problem_line_in(language: Locale, problem: &lcc_catalog::rules::Problem) -> String {
    let at: Vec<String> = problem.path.iter().map(|i| i.saturating_add(1).to_string()).collect();
    // a problem with the album filter around the rules has no rule to point at
    if at.is_empty() {
        return problem_text_in(language, problem);
    }
    format!("#{} {}", at.join("."), problem_text_in(language, problem))
}

/// What people read for smart-album choice `id` of `field` ("Gemeinfrei" for `publicDomain` in
/// German); an id the field doesn't know, as written.
pub fn choice_text<'a>(field: &str, id: &'a str) -> &'a str {
    choice_text_in(language(), field, id)
}

fn choice_text_in<'a>(language: Locale, field: &str, id: &'a str) -> &'a str {
    lcc_catalog::rules::choice_label(field, id).map_or(id, |label| tr_in(language, label))
}

/// What people read for a smart-album yes/no value ("Ja" / "Nein" in German).
pub fn bool_text(b: bool) -> &'static str {
    bool_text_in(language(), b)
}

/// The context of a smart-album yes/no value ([`tr_ctx_in`]).
const BOOL_CONTEXT: &str = "smart album value";

fn bool_text_in(language: Locale, b: bool) -> &'static str {
    tr_ctx_in(language, BOOL_CONTEXT, lcc_catalog::rules::bool_label(b))
}

/// A rule summary for display, keeping free-text rule values verbatim.
pub fn rules_label(rules: &lcc_catalog::RuleSet, catalog: &lcc_catalog::Catalog) -> String {
    fn describe(rules: &lcc_catalog::RuleSet, catalog: &lcc_catalog::Catalog, depth: usize) -> String {
        use lcc_catalog::{
            Match, Rule,
            rules::{FIELDS, Kind, ops_for},
        };
        if depth >= 16 {
            return tr("Nested group").to_string();
        }
        let parts: Vec<String> = rules
            .rules
            .iter()
            .map(|rule| match rule {
                Rule::Group { group } => format!("({})", describe(group, catalog, depth + 1)),
                Rule::Field { field, op, value } => {
                    let Some((_, label, kind)) = FIELDS.iter().find(|entry| entry.0 == field) else { return field.clone() };
                    let operator = ops_for(*kind).iter().find(|entry| entry.0 == op).map_or(op.as_str(), |entry| entry.1);
                    let text = match kind {
                        // the album's name, as people know it (a user's name is never translated)
                        Kind::Album => lcc_catalog::rules::album_name(value, Some(catalog)),
                        Kind::Choice(_) => value.as_str().map_or_else(|| value.to_string(), |id| choice_text(field, id).to_string()),
                        Kind::Bool => lcc_catalog::rules::bool_value(value).map_or_else(|| value.to_string(), |b| bool_text(b).to_string()),
                        _ if matches!(op.as_str(), "inLast" | "notInLast") => format!(
                            "{} {}",
                            value.get("n").unwrap_or(&serde_json::Value::Null),
                            tr(value.get("unit").and_then(serde_json::Value::as_str).unwrap_or("days"))
                        ),
                        _ => value.as_str().map(str::to_string).unwrap_or_else(|| if value.is_null() { String::new() } else { value.to_string() }),
                    };
                    format!("{} {} {text}", tr(label), tr(operator)).trim().to_string()
                }
            })
            .collect();
        let join = match rules.mode {
            Match::All => tr("and"),
            Match::Any | Match::None => tr("or"),
        };
        let text = parts.join(&format!(" {join} "));
        if rules.mode == Match::None { format!("{} ({text})", tr("none of")) } else { text }
    }
    // Preserve the established source-language summary.
    if language() == Locale::En { rules.describe_with(catalog) } else { describe(rules, catalog, 0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each language through its menu command and the control channel's locale tags, saved by its
    /// code: built-in labels follow it, user values and ids don't, the window paints its text and
    /// the bundled fonts have its letters.
    #[test]
    fn languages_switch_persist_and_keep_user_values() {
        use crate::control::{ControlRequest, Outcome};
        let languages: [(Locale, &str, &[&str], &str); 8] = [
            (Locale::Cs, "czech", &["cs", "cs-CZ", "cs_CZ.UTF-8", "cs-Latn-CZ"], "áčďéěíňóřšťúůýžÁČĎÉĚÍŇÓŘŠŤÚŮÝŽ„“"),
            (Locale::Id, "indonesian", &["id", "id-ID", "id_ID.UTF-8", "id-Latn-ID"], ""),
            (Locale::Sv, "swedish", &["sv", "sv-SE", "sv_SE.UTF-8", "sv-FI"], "åäöÅÄÖé”’"),
            (Locale::Uk, "ukrainian", &["uk", "uk-UA", "uk_UA.UTF-8", "uk-Cyrl-UA"], ""),
            (Locale::Ka, "georgian", &["ka", "ka-GE", "ka_GE.UTF-8"], ""),
            (Locale::It, "italian", &["it", "it-IT", "it_IT.UTF-8", "it-Latn-IT", "it-CH"], ""),
            (Locale::Fi, "finnish", &["fi", "fi-FI", "fi_FI.UTF-8", "fi-Latn-FI"], ""),
            (Locale::Vi, "vietnamese", &["vi", "vi-VN", "vi_VN", "vi_VN.UTF-8"], ""),
        ];
        for (locale, command, tags, letters) in languages {
            let command = format!("app.language.{command}");
            let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), Default::default());
            app.run(&command, serde_json::json!({})).unwrap();
            assert_eq!(app.ui.language, locale);
            assert_eq!(language(), locale);
            assert_eq!(crate::menubar::checked(&app, &command), Some(true));
            assert_eq!(builtin_label("Warm Glow", false), "Warm Glow");
            assert_eq!(builtin_label("Warm Glow", true), locale.tr("Warm Glow"));
            assert_eq!(tr("photo-{title}.jpg"), "photo-{title}.jpg");
            assert_eq!(tr("develop.set"), "develop.set");
            let saved = serde_json::to_string(&app.ui).unwrap();
            assert!(saved.contains(&format!(r#""language":"{}""#, locale.code())), "{saved}");
            assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, locale);
            let ctx = egui::Context::default();
            for tag in tags {
                assert_eq!(Locale::parse_tag(tag), Some(locale), "{tag}");
                let (request, _) = ControlRequest::new("ui.set", serde_json::json!({"language": tag}));
                let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &request) else { panic!("expected reply") };
                assert_eq!(reply["ok"], true);
                assert_eq!(language(), locale);
            }
            let text = painted_text(&ctx, &mut app, locale);
            // (not the menu titles: a language whose titles don't fit shows one Menu button)
            assert!(text.contains(locale.tr("Exposure")), "{locale:?}: {text}");
            ctx.fonts_mut(|fonts| {
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                    let font = egui::FontId::new(13.0, family);
                    for ch in letters.chars() {
                        assert!(fonts.has_glyph(&font, ch), "{locale:?}: missing glyph {ch}");
                    }
                }
            });
        }
        set_language(Locale::En);
    }

    #[test]
    fn sentence_case_keeps_georgian_lowercase() {
        assert_eq!(upper_first("its input is not an image"), "Its input is not an image");
        assert_eq!(upper_first("ფაილი ვერ იკითხება"), "ფაილი ვერ იკითხება");
        assert_eq!(upper_first("ᲥᲐᲠᲗᲣᲚᲘ"), "ᲥᲐᲠᲗᲣᲚᲘ");
        assert_eq!(upper_first(""), "");
        assert!(capitalizes('a') && capitalizes('ж') && !capitalizes('ქ') && !capitalizes('Ⴀ') && !capitalizes('ⴀ'));
    }

    #[test]
    fn every_language_has_a_name_a_script_and_its_own_code() {
        assert!(crate::i18n::Locale::En.catalog().is_empty(), "the source language needs no catalog");
        for language in Locale::ALL {
            assert!(!language.name().is_empty(), "{language:?} has a name");
            assert!(!language.script().is_empty(), "{language:?} has a script");
            assert_eq!(Locale::parse(language.code()), Some(*language), "{language:?} parses its own code");
        }
    }

    /// The fork's own catalogs (`locales/fork/*.json`) are read by `build.rs` at compile time, so a
    /// merge that mangles one fails the build with a JSON error. This says which file, and what is
    /// wrong with it, in the test that already knows the name.
    #[test]
    fn fork_catalogs_are_valid_json() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales").join("fork");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("locales/fork") {
            let path = entry.expect("a catalog").path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            seen += 1;
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let messages: std::collections::BTreeMap<String, String> =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(!messages.is_empty(), "{}: no messages", path.display());
            for (key, value) in &messages {
                assert!(!key.is_empty() && !value.is_empty(), "{}: empty message for {key:?}", path.display());
            }
        }
        assert!(seen >= 2, "found only {seen} fork catalog(s) in {}", dir.display());
    }

    /// Every catalog file is a JSON object of strings, each key once: a malformed file would load as
    /// no catalog, and a repeated key silently drops one of its translations.
    #[test]
    fn catalogs_load() {
        let files = catalog_files();
        assert_eq!(files.len(), 2 * (Locale::ALL.len() - 1), "a plain and a formats catalog per translation");
        for path in files {
            let repeated = repeated_keys(&path);
            assert_eq!(repeated, Ok(Vec::new()), "{}", path.display());
        }
    }

    /// `locales/*.json`, sorted.
    fn catalog_files() -> Vec<std::path::PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort();
        files
    }

    /// The keys a catalog file names more than once, or why it isn't an object of strings.
    fn repeated_keys(path: &std::path::Path) -> Result<Vec<String>, String> {
        struct Repeated;
        impl<'de> serde::de::Visitor<'de> for Repeated {
            type Value = Vec<String>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object of strings")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Vec<String>, A::Error> {
                let (mut seen, mut repeated) = (std::collections::BTreeSet::new(), Vec::new());
                while let Some((key, _)) = map.next_entry::<String, String>()? {
                    if !seen.insert(key.clone()) {
                        repeated.push(key);
                    }
                }
                Ok(repeated)
            }
        }
        let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        serde::Deserializer::deserialize_map(&mut serde_json::Deserializer::from_str(&text), Repeated).map_err(|error| error.to_string())
    }

    /// A translation carries the placeholders of the message it translates.
    #[test]
    fn catalogs_agree_on_placeholders() {
        let fields = |text: &str| {
            let mut out: Vec<String> = Vec::new();
            let mut rest = text;
            while let Some(start) = rest.find('{') {
                let Some(end) = rest[start..].find('}') else { break };
                let name = rest[start + 1..start + end].split(':').next().unwrap_or("").to_string();
                // brand placeholders are filled in at load time, so only the key still has them
                if !matches!(name.as_str(), "app" | "cli" | "env" | "upstream") {
                    out.push(name);
                }
                rest = &rest[start + end + 1..];
            }
            out.sort();
            out
        };
        for language in Locale::ALL {
            for (key, value) in language.catalog() {
                assert_eq!(fields(key), fields(value), "{language:?}: {key:?} -> {value:?}");
            }
        }
    }

    /// Locale tags are matched leniently, so a system locale reaches a shipped language.
    /// The message box for a desktop start that failed (issue #260) names the error and the log
    /// file, in the UI language.
    #[test]
    fn startup_failure_message_names_the_error_and_the_log() {
        let log = format!("/home/a/.config/app/logs/{}.log", lcc_brand::BINARY);
        let (title, text) = startup_failed_message("no adapter", Some(&log));
        assert_eq!(title, format!("{} could not start", lcc_brand::DISPLAY_NAME));
        assert!(text.contains("no adapter") && text.ends_with(&log), "{text}");
        assert!(!startup_failed_message("no adapter", None).1.contains("log file"));
    }

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
        assert_eq!(Locale::parse_tag("fr"), Some(Locale::Fr));
        assert_eq!(Locale::parse_tag("fr-FR"), Some(Locale::Fr));
        assert_eq!(Locale::parse_tag("fr_FR.UTF-8"), Some(Locale::Fr));
        for tag in ["de", "de-DE", "de_AT.UTF-8", "de-CH"] {
            assert_eq!(Locale::parse_tag(tag), Some(Locale::De), "{tag}");
        }
        for tag in ["ru", "ru-RU", "ru_RU.UTF-8", "ru-UA"] {
            assert_eq!(Locale::parse_tag(tag), Some(Locale::Ru), "{tag}");
        }
        for tag in ["es", "es-ES", "es_MX.UTF-8", "es-419"] {
            assert_eq!(Locale::parse_tag(tag), Some(Locale::Es), "{tag}");
        }
        for tag in ["vi", "vi-VN", "vi_VN", "vi_VN.UTF-8"] {
            assert_eq!(Locale::parse_tag(tag), Some(Locale::Vi), "{tag}");
        }
        assert_eq!(Locale::parse_tag("xx"), None);
    }

    /// The web host's "Auto" (issue #2085): the browser's `navigator.languages` in order, first
    /// shipped language wins, unsupported tags are skipped, an empty or foreign list stays English
    /// ([`default_language`] fallback).
    #[test]
    fn locale_from_tags_takes_the_first_shipped_language() {
        assert_eq!(locale_from_tags(&[]), None);
        assert_eq!(locale_from_tags(&["xx-XX".into()]), None);
        assert_eq!(locale_from_tags(&["it-IT".into()]), Some(Locale::It));
        assert_eq!(locale_from_tags(&["xx".into(), "de-DE".into(), "fr".into()]), Some(Locale::De));
        assert_eq!(locale_from_tags(&["zh-TW".into(), "en".into()]), Some(Locale::ZhHant));
        assert_eq!(locale_from_tags(&["ja_JP.UTF-8".into(), "en".into()]), Some(Locale::Ja));
        assert_eq!(locale_from_tags(&["pt".into(), "pt-BR".into()]), Some(Locale::PtBr));
    }

    /// Format strings carry their values in every language, and a language without a translation
    /// falls back to the English format rather than dropping the values.
    #[test]
    fn formats_render_in_every_language() {
        // Every language's format string is checked by `format!` against the call site's values.
        set_language(Locale::En);
        assert_eq!(tr_format!("Imported {} photo{}", 12, "s"), "Imported 12 photos");
        assert_eq!(tr_format!("Exported {ok} of {total} photo{}", "s", ok = 4, total = 12), "Exported 4 of 12 photos");
        // A message no catalog has formats its English source.
        assert_eq!(tr_format!("{} of {} (no catalog has this)", 1, 2), "1 of 2 (no catalog has this)");
        // Format specs survive translation: precision, sign, and values a language reorders; user
        // text (an album name with braces and CJK) and counts are never dropped.
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
            let name = "Color 1234.56 {title} 日本語";
            let added = tr_format!("Added {n} photo{} to “{}”", "s", name, n = 22);
            assert!(added.contains(name) && added.contains("22"), "{language:?}: {added}");
            for n in [0, 1, 2, 5, 22] {
                let imported = tr_format!("Imported {} photo{}", n, if n == 1 { "" } else { "s" });
                assert!(imported.contains(&n.to_string()), "{language:?}: {imported}");
            }
            // The trailing argument is queue progress, not an English plural suffix.
            let queued = tr_format!(" · {queued} waiting", queued = 5);
            let denoising = tr_format!("Denoising {name}: {done} of {total} tiles{}", queued, name = "IMG.CR3", done = 1, total = 4);
            assert!(denoising.contains("IMG.CR3") && denoising.contains('5'), "{language:?}: {denoising}");
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
        ctx.set_fonts(crate::theme::font_definitions(lcc_engine::craft_fonts()));
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
                lcc_engine::craft_fonts().iter().filter(|font| font.serves(language.script())).map(|font| font.family).collect();
            faces.dedup();
            if faces.is_empty() {
                lcc_testkit::skip!(Fonts, "{}: built without a craft-fonts face for {}", language.code(), language.script());
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
        ctx.set_fonts(crate::theme::font_definitions(lcc_engine::craft_fonts()));
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

    /// Symbols a translation must not carry, because the craft-fonts faces of at least one shipped
    /// script have no glyph for them and egui's bundled fonts do not either: the menu triangles,
    /// and the heavy multiplication X (U+2715) that Noto Sans Georgian lacks — `×` (U+00D7) reads
    /// the same and every face has it.
    const UNPAINTABLE: [char; 3] = ['▸', '▾', '✕'];

    #[test]
    fn translated_catalogs_do_not_use_missing_menu_triangles() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        // upstream's catalogs and the fork's rows the build merges over them (`locales/fork/`)
        let entries = std::fs::read_dir(&dir).unwrap().chain(std::fs::read_dir(dir.join("fork")).unwrap());
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let messages: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for (source, text) in messages {
                for bad in UNPAINTABLE {
                    assert!(!text.contains(bad), "{}: {source:?} uses the unpaintable {bad:?}", path.display());
                }
            }
        }
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
            ("app.language.french", Locale::Fr),
            ("app.language.spanish", Locale::Es),
            ("app.language.german", Locale::De),
            ("app.language.russian", Locale::Ru),
            ("app.language.ukrainian", Locale::Uk),
            ("app.language.polish", Locale::Pl),
            ("app.language.czech", Locale::Cs),
            ("app.language.italian", Locale::It),
            ("app.language.georgian", Locale::Ka),
            ("app.language.swedish", Locale::Sv),
            ("app.language.finnish", Locale::Fi),
            ("app.language.indonesian", Locale::Id),
            ("app.language.vietnamese", Locale::Vi),
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

    /// A word with two meanings in the UI is asked for with a context in each place, and a contextual
    /// entry changes only its own place: where a language has `focus|Tracking`, the Focus row shows it
    /// while the book's letter-spacing slider keeps the plain translation, and the other way round with
    /// `book text|Tracking`. A language without a contextual entry shows the plain translation there.
    /// (Every place asks with its key: `every_contextual_lookup_is_asked_for_in_the_code`.)
    #[test]
    fn contextual_entries_change_only_their_own_place() {
        let tracking_area = lcc_meta::FocusPoint {
            rect: lcc_geom::Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 },
            mode: None,
            area: Some("Tracking".into()),
            tracking: None,
        };
        let (mut focus_only, mut book_only) = (0, 0);
        for language in Locale::ALL {
            let catalog = language.catalog();
            let plain = language.tr("Tracking");
            let (focus, book) = (catalog.get("focus|Tracking"), catalog.get("book text|Tracking"));
            set_language(*language);
            let shown = crate::panels::metadata::focus_text(Some(&tracking_area));
            set_language(Locale::En);
            assert_eq!(shown.as_deref(), Some(focus.map_or(plain, String::as_str)), "{}: the Focus row", language.code());
            assert_eq!(tr_in(*language, "book text|Tracking"), book.map_or(plain, String::as_str), "{}: the book's slider", language.code());
            focus_only += usize::from(focus.is_some_and(|f| f != plain) && book.is_none());
            book_only += usize::from(book.is_some_and(|b| b != plain) && focus.is_none());
        }
        assert!(focus_only > 0 && book_only > 0, "some language words each place apart from the plain translation ({focus_only}, {book_only})");
        assert_eq!(tr_in(Locale::En, "book text|Tracking"), "Tracking", "English shows the word as is");
        // every pair: its own entry where the language has one, else the plain message; and each word's
        // places read differently in some language, or the contexts change nothing
        for (context, source) in CONTEXTUAL {
            let key = format!("{context}|{source}");
            for language in Locale::ALL {
                let expected = language.catalog().get(&key).map_or(language.tr(source), String::as_str);
                assert_eq!(tr_in(*language, &key), expected, "{}: {key:?}", language.code());
            }
            for other in CONTEXTUAL.iter().filter(|(c, s)| s == source && c != context).map(|(c, s)| format!("{c}|{s}")) {
                assert!(Locale::ALL.iter().any(|l| tr_in(*l, &key) != tr_in(*l, &other)), "{key:?} and {other:?} read alike in every language");
            }
        }
    }

    /// Smart-album choice values read as words in every language (English "Public Domain"), never as
    /// rule ids like `publicDomain`; an id the field doesn't know shows as written. A yes/no value
    /// reads Yes / No, or the language's contextual entry for it where it has one.
    #[test]
    fn choice_values_read_as_words_in_every_language() {
        use lcc_catalog::rules::{FIELDS, Kind, bool_label};
        assert_eq!(choice_text_in(Locale::En, "copyrightStatus", "publicDomain"), "Public Domain");
        assert_eq!((bool_text_in(Locale::En, true), bool_text_in(Locale::En, false)), ("Yes", "No"));
        assert_eq!(tr_ctx_in(Locale::En, BOOL_CONTEXT, "Yes"), "Yes");
        for language in Locale::ALL {
            assert_eq!(choice_text_in(*language, "copyrightStatus", "someday"), "someday");
            assert_eq!(tr_ctx_in(*language, "nope", "Yes"), language.tr("Yes"), "no contextual entry: the plain message");
            for b in [true, false] {
                let contextual = language.catalog().get(&format!("{BOOL_CONTEXT}|{}", bool_label(b)));
                assert_eq!(bool_text_in(*language, b), contextual.map_or(language.tr(bool_label(b)), String::as_str));
            }
            for (field, _, kind) in FIELDS {
                let Kind::Choice(choices) = kind else { continue };
                for (id, _) in *choices {
                    assert_ne!(choice_text_in(*language, field, id), *id, "{}: {field} {id}", language.code());
                }
            }
        }
        // a contextual entry names a real value (a typo would silently fall back to the plain Yes / No)
        let values = [bool_label(true), bool_label(false)];
        for language in Locale::ALL {
            for key in language.catalog().keys() {
                if let Some(value) = key.strip_prefix(BOOL_CONTEXT).and_then(|k| k.strip_prefix('|')) {
                    assert!(values.contains(&value), "{}: {key:?} isn't a yes/no value", language.code());
                }
            }
        }
    }

    /// A rule's problem reads as the field's name and the issue, in the editor's language, joined by
    /// a full-width colon in Japanese and Chinese; the sidebar's list puts the rule's position first.
    #[test]
    fn problems_read_in_every_language() {
        use lcc_catalog::rules::Issue;
        let cat = lcc_catalog::Catalog::new();
        let problems =
            |rules: serde_json::Value| serde_json::from_value::<lcc_catalog::RuleSet>(serde_json::json!({"rules": rules})).unwrap().check(&cat);
        let p = problems(serde_json::json!([{"field": "rating", "op": "gte", "value": 3}, {"field": "title", "op": "contains", "value": ""}]));
        assert_eq!(problem_text_in(Locale::En, &p[0]), "Title: needs something to look for (or use “is empty”)");
        assert_eq!(problem_line_in(Locale::En, &p[0]), "#2 Title: needs something to look for (or use “is empty”)");
        for language in Locale::ALL {
            let colon = if matches!(language.script(), "Jpan" | "Hans" | "Hant") { "：" } else { ": " };
            let text = format!("{}{colon}{}", language.tr("Title"), language.tr(p[0].issue.text()));
            assert_eq!(problem_text_in(*language, &p[0]), text);
            assert_eq!(problem_line_in(*language, &p[0]), format!("#2 {text}"));
        }
        // a problem with the album filter itself has no rule position to show
        let filter_loop =
            lcc_catalog::rules::Problem { path: Vec::new(), field: Some("album".into()), issue: Issue::AlbumLoop, message: String::new() };
        assert!(problem_line_in(Locale::En, &filter_loop).starts_with("Collection: "), "{}", problem_line_in(Locale::En, &filter_loop));
        let g = problems(serde_json::json!([{"group": {"rules": []}}]));
        assert_eq!(problem_text_in(Locale::En, &g[0]), "This group is empty: add a rule or remove it.");
    }

    #[test]
    fn german_switches_via_menu_and_control_and_persists() {
        use crate::control::{ControlRequest, Outcome};
        let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        app.run("app.language.german", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Locale::De);
        assert_eq!(language(), Locale::De);
        assert_eq!(crate::menubar::checked(&app, "app.language.german"), Some(true));
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert!(saved.contains(r#""language":"de""#));
        assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, Locale::De);
        let ctx = egui::Context::default();
        for code in ["de-DE", "de_AT.UTF-8", "de-CH"] {
            let (req, _) = ControlRequest::new("ui.set", serde_json::json!({"language": code}));
            let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &req) else { panic!("expected reply") };
            assert_eq!(reply["ok"], true);
            assert_eq!(language(), Locale::De);
        }
        assert_eq!(builtin_label("Warm Glow", false), "Warm Glow");
        assert_eq!(builtin_label("Warm Glow", true), Locale::De.tr("Warm Glow"));
        assert_eq!(tr("my-photo.jpg"), "my-photo.jpg");
        let rules = lcc_catalog::RuleSet {
            mode: lcc_catalog::Match::All,
            rules: vec![lcc_catalog::Rule::Field { field: "keywords".into(), op: "contains".into(), value: serde_json::json!("Color") }],
        };
        let mut catalog = lcc_catalog::Catalog::new();
        catalog.apply(lcc_catalog::Op::AddAlbum { album: lcc_catalog::Album::new(lcc_catalog::AlbumId(4), "Ausgeschlossen") }).unwrap();
        // "Color" is a message too, but a rule's free-text value stays as written
        let keywords = lcc_catalog::rules::field_label("keywords").unwrap_or("keywords");
        assert_eq!(rules_label(&rules, &catalog), format!("{} {} Color", Locale::De.tr(keywords), Locale::De.tr("contains")));
        let album = lcc_catalog::RuleSet {
            mode: lcc_catalog::Match::All,
            rules: vec![lcc_catalog::Rule::Field { field: "album".into(), op: "isNot".into(), value: serde_json::json!(4) }],
        };
        assert!(rules_label(&album, &catalog).ends_with(" “Ausgeschlossen”"), "{}", rules_label(&album, &catalog));
        let rules = lcc_catalog::RuleSet {
            mode: lcc_catalog::Match::All,
            rules: vec![lcc_catalog::Rule::Field { field: "copyrightStatus".into(), op: "is".into(), value: serde_json::json!("publicDomain") }],
        };
        let public_domain = format!(" {}", choice_text_in(Locale::De, "copyrightStatus", "publicDomain"));
        assert!(rules_label(&rules, &catalog).ends_with(&public_domain), "{}", rules_label(&rules, &catalog));
        for value in [serde_json::json!(false), serde_json::json!("false")] {
            let rules = lcc_catalog::RuleSet {
                mode: lcc_catalog::Match::All,
                rules: vec![lcc_catalog::Rule::Field { field: "edited".into(), op: "is".into(), value }],
            };
            let no = format!(" {}", bool_text_in(Locale::De, false));
            assert!(rules_label(&rules, &catalog).ends_with(&no), "{}", rules_label(&rules, &catalog));
        }
        let both = lcc_catalog::Filter {
            label: Some(lcc_catalog::ColorLabel::Red),
            labels: vec![lcc_catalog::ColorLabel::Blue, lcc_catalog::ColorLabel::Red],
            ..Default::default()
        };
        let name = |label| color_label(&app.session.catalog, label);
        let (red, blue, green) = (name(lcc_catalog::ColorLabel::Red), name(lcc_catalog::ColorLabel::Blue), name(lcc_catalog::ColorLabel::Green));
        let (label, and, or) = (tr("Label"), tr("and"), tr("or"));
        assert_eq!(filter_label(&both, &app.session.catalog), format!("{label}: {red}"));
        let both = lcc_catalog::Filter { labels: vec![lcc_catalog::ColorLabel::Blue], ..both };
        assert_eq!(filter_label(&both, &app.session.catalog), format!("{label}: {red} {and} {blue}"));
        let both = lcc_catalog::Filter { labels: vec![lcc_catalog::ColorLabel::Blue, lcc_catalog::ColorLabel::Green], ..both };
        assert_eq!(filter_label(&both, &app.session.catalog), format!("{label}: {red} {and} ({blue} {or} {green})"));
        let filter = lcc_catalog::Filter { labels: vec![lcc_catalog::ColorLabel::Red, lcc_catalog::ColorLabel::Blue], ..Default::default() };
        let blue = color_label(&app.session.catalog, lcc_catalog::ColorLabel::Blue);
        let red = color_label(&app.session.catalog, lcc_catalog::ColorLabel::Red);
        let label = filter_label(&filter, &app.session.catalog);
        assert!(label.contains(&red) && label.contains(&blue), "{label}");
        // a label set's own name is the user's
        app.run("label.setNames", serde_json::json!({"names": {"red": "Color"}})).unwrap();
        let label = filter_label(&filter, &app.session.catalog);
        assert!(label.contains("Color") && !label.contains(&red) && label.contains(&blue), "{label}");
        set_language(Locale::En);
    }

    /// The languages written in Latin or Cyrillic letters need no craft-fonts face: their panels and
    /// dialogs paint translated, and the bundled fonts have every accented letter their catalogs use.
    #[test]
    fn latin_and_cyrillic_languages_paint_panels_dialogs_and_letters_with_bundled_fonts() {
        // (language, letters it must have, the ranges of its catalog's characters checked as well)
        let languages: [(Locale, &str, &[(char, char)]); 5] = [
            (Locale::De, "ÄÖÜäöüß„“", &[]),
            (Locale::Uk, "ҐґЄєІіЇї’«»", &[('\u{0400}', '\u{04ff}')]),
            (Locale::It, "àèéìòùÀÈÉÌÒÙ’“”«»", &[('\u{00c0}', '\u{017f}')]),
            (Locale::Fi, "äöåÄÖÅ”–", &[('\u{00c0}', '\u{017f}')]),
            (Locale::Vi, "ĂăÂâÊêÔôƠơƯưĐđáàảãạéèẻẽẹíìỉĩịóòỏõọúùủũụýỳỷỹỵ", &[('\u{00c0}', '\u{024f}'), ('\u{1ea0}', '\u{1ef9}')]),
        ];
        let ctx = fonts_ctx(&[]);
        let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        app.ui.left_panel = true;
        show_my_photos(&mut app);
        for (locale, letters, ranges) in languages {
            app.ui.dialog = None;
            let text = painted_text(&ctx, &mut app, locale);
            assert!(text.contains(locale.tr("My Photos")) && text.contains(locale.tr("All Photographs")), "{locale:?}: {text}");
            for (command, title) in [("app.about", "About {app}"), ("app.shortcuts", "Keyboard Shortcuts"), ("app.settings", "Settings")] {
                app.ui.dialog = None;
                app.run(command, serde_json::json!({})).unwrap();
                let text = painted_text(&ctx, &mut app, locale);
                assert!(text.contains(locale.tr(title)), "{locale:?} {command}: {text}");
            }
            ctx.fonts_mut(|fonts| {
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                    let font = egui::FontId::new(13.0, family);
                    let used =
                        locale.catalog().values().flat_map(|text| text.chars()).filter(|ch| ranges.iter().any(|(a, b)| (*a..=*b).contains(ch)));
                    for ch in letters.chars().chain(used) {
                        assert!(fonts.has_glyph(&font, ch), "{locale:?}: missing glyph {ch}");
                    }
                }
            });
        }
        app.ui.dialog = None;
        app.ui.view = crate::state::ViewMode::People;
        let text = painted_text(&ctx, &mut app, Locale::De);
        assert!(text.contains(Locale::De.tr("Named People")), "{text}");
        set_language(Locale::En);
    }

    #[test]
    fn german_ukrainian_italian_and_finnish_buttons_fit_the_bottom_bar_and_export_dialog() {
        fn collect(shape: &egui::epaint::Shape, bounds: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(shape) => {
                    bounds.push((shape.galley.job.text.clone(), egui::Rect::from_min_size(shape.pos, shape.galley.size())))
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect(shape, bounds)),
                _ => {}
            }
        }
        for language in [Locale::De, Locale::Uk, Locale::It, Locale::Fi] {
            let ctx = fonts_ctx(&[]);
            let services = crate::Services { pick_folder: Some(Box::new(|| None)), ..Default::default() };
            let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), services);
            app.ui.language = language;
            let mut bounds = Vec::new();
            for export in [false, true] {
                if export {
                    app.run("dialog.export", serde_json::json!({})).unwrap();
                    // (a specific folder: the Folder row with its Choose… button)
                    if let Some(crate::state::Dialog::Export { opts, .. }) = &mut app.ui.dialog {
                        opts.destination_mode = lcc_engine::export::DestinationMode::SpecificFolder;
                    }
                }
                for frame in 0..4 {
                    let input = crate::headless::HeadlessView::raw_input(egui::vec2(1600.0, 1000.0), 1.0, frame as f64 / 60.0, vec![]);
                    let mut out = ctx.run_ui(input, |ui| {
                        app.logic(ui.ctx());
                        app.ui(ui);
                    });
                    out.textures_delta.clear();
                    bounds.clear();
                    for shape in out.shapes {
                        collect(&shape.shape, &mut bounds);
                    }
                }
                let rect = |id: &str| app.widgets.iter().find(|entry| entry.0 == id).unwrap_or_else(|| panic!("no widget {id}")).1;
                if export {
                    let dialog = rect("dialog:window");
                    for id in ["button:exportNamingTags", "button:exportChooseFolder", "button:exportSavePreset"] {
                        // (sideways: the options below the fold scroll into view)
                        let r = rect(id);
                        assert!(dialog.left() <= r.left() && r.right() <= dialog.right(), "{id} spills outside {dialog:?}: {r:?}");
                    }
                } else {
                    let button = rect("button:copySettings");
                    let (_, text) = bounds.iter().find(|entry| entry.0 == language.tr("Copy Edit Settings")).unwrap();
                    assert!(button.contains_rect(*text), "{language:?} copy caption overflows its button: {text:?} vs {button:?}");
                    assert!(!button.intersects(rect("icon:copyGear")));
                }
            }
        }
        set_language(Locale::En);
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
        set_language(Locale::En);
    }

    /// Upstream's My Photos sidebar is one of Develop's Classic panels now, hidden until it is
    /// asked for: the tests that look for its labels (Meine Fotos, Мої фото, My Photos) show it
    /// first, so what they read is on screen.
    fn show_my_photos(app: &mut crate::LccApp) {
        app.ui.hidden_panels.retain(|p| *p != crate::module::PanelId::Sources);
    }

    /// The text the whole window paints over a few frames in `language`.
    fn painted_text(ctx: &egui::Context, app: &mut crate::LccApp, language: Locale) -> String {
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
        let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        app.ui.left_panel = true;
        show_my_photos(&mut app);
        let ids = |app: &crate::LccApp| crate::menus::menu_entries(app).into_iter().map(|entry| entry.id).collect::<Vec<_>>();
        let english_ids = ids(&app);
        for language in Locale::ALL {
            let text = painted_text(&ctx, &mut app, *language);
            // (Classic names the catalog's row All Photographs, as Lightroom Classic)
            for label in ["My Photos", "All Photographs"] {
                assert!(text.contains(language.tr(label)), "{language:?}: {label} -> {:?} not in\n{text}", language.tr(label));
            }
            assert_eq!(app.font_language, *language, "the fonts follow the language");
            assert_eq!(ids(&app), english_ids, "{language:?}: command ids are presentation-independent");
        }
        set_language(Locale::En);
    }

    /// The UI families, after one frame so the font definitions are loaded.
    fn fonts_ctx(craft: &'static [lcc_engine::CraftFont]) -> egui::Context {
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
            if !lcc_engine::craft_fonts().iter().any(|font| font.serves(language.script())) {
                lcc_testkit::skip!(Fonts, "{}: built without a craft-fonts face for {}", language.code(), language.script());
                continue;
            }
            set_language(language);
            let ctx = fonts_ctx(lcc_engine::craft_fonts());
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
                assert!(lcc_brand::DISPLAY_NAME.chars().all(|ch| fonts.has_glyph(&font, ch)), "{font:?}");
            }
        });
        let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), Default::default());
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
    /// localised headings; ids and user text are untouched.
    #[test]
    fn traditional_chinese_switches_through_menu_and_control_and_persists() {
        use crate::control::{ControlRequest, Outcome};
        let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        app.run("app.language.traditionalChinese", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Locale::ZhHant);
        // Applied at once, not on the next frame.
        assert_eq!(language(), Locale::ZhHant);
        assert_eq!(source_label(lcc_engine::LibrarySource::All, &app.session.catalog), Locale::ZhHant.tr("All Photographs"));
        assert_eq!(date_group_label("", false), Locale::ZhHant.tr("Unknown Date"));
        assert_eq!(display_time("a-user-value"), "a-user-value");
        assert_eq!(crate::menubar::checked(&app, "app.language.traditionalChinese"), Some(true));
        assert_eq!(crate::menubar::checked(&app, "app.language.english"), Some(false));
        assert_eq!(tr("my-photo.jpg"), "my-photo.jpg");
        for id in ["album.addPhotos", "metadata.applyPreset", "label.applySet"] {
            assert_eq!(crate::menubar::display_item_label(id, &serde_json::json!({"id": 1}), "Color"), "Color");
        }
        assert_eq!(crate::menubar::display_item_label("view.photoGrid", &serde_json::Value::Null, "Color"), Locale::ZhHant.tr("Color"));
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
        let ids = |app: &crate::LccApp| crate::menus::menu_entries(app).into_iter().map(|entry| entry.id).collect::<Vec<_>>();
        let chinese_ids = ids(&app);
        app.run("app.language.english", serde_json::json!({})).unwrap();
        assert_eq!(tr("File"), "File");
        assert_eq!(chinese_ids, ids(&app));
    }

    /// Dates follow every language's own patterns (English keeps the catalog's wording); a key that
    /// is not a date never panics and never shows a raw pattern.
    #[test]
    fn date_headings_in_every_language() {
        set_language(Locale::En);
        assert_eq!(date_group_label("2026-09-20", false), "Sunday, 20 September 2026");
        assert_eq!(date_group_label("2026-09-20", true), "Sunday, 20");
        assert_eq!(date_group_label("2026-09", true), "September");
        assert_eq!(display_time("2026-09-20T16:04:05"), lcc_catalog::dates::display_time("2026-09-20T16:04:05"));
        assert_eq!((display_time("1983-04"), display_time("1983")), ("April 1983".to_string(), "1983".to_string()));
        for language in Locale::ALL {
            set_language(*language);
            if *language != Locale::En {
                let day = date_group_label("2026-09-20", false);
                assert!(day.contains("20") && day.contains("2026") && day.contains(tr("Sunday")), "{language:?}: {day}");
                assert!(date_group_label("2026-09-20", true).contains("20"), "{language:?}");
                assert!(date_group_label("2026-09", false).contains("2026"), "{language:?}");
                assert!(date_group_label("2026", false).contains("2026"), "{language:?}");
                assert_eq!(display_time("2026-09-20T16:04:05"), format!("{day} 16:04:05"), "{language:?}");
            }
            // a capture time stated to the year or the month alone shows as such
            assert_eq!(display_time("1983-04"), date_group_label("1983-04", false), "{language:?}");
            assert_eq!(display_time("1983"), date_group_label("1983", false), "{language:?}");
            for key in ["", "abcd", "2026-xx", "2026-13", "2026-09-xx", "２０２６-09"] {
                assert!(!date_group_label(key, false).contains('{'), "{language:?}: {key:?}");
                assert!(!date_group_label(key, true).contains('{'), "{language:?}: {key:?}");
            }
        }
        set_language(Locale::En);
    }

    /// Every language shows a capture time to the same second precision English does: sub-second
    /// digits and a time-zone suffix in the metadata are not shown (#743). A value that doesn't
    /// normalize is shown as it is in every language.
    #[test]
    fn display_time_shows_whole_seconds_in_every_language() {
        set_language(Locale::En);
        assert_eq!(display_time("2026-09-20T16:04:05.123+02:00"), "September 20, 2026 at 4:04:05 PM");
        for language in Locale::ALL {
            set_language(*language);
            if *language == Locale::En {
                continue;
            }
            let shown = display_time("2026-09-20T16:04:05.123+02:00");
            assert!(!shown.contains("123"), "{language:?}: {shown}");
            assert!(!shown.contains("+02"), "{language:?}: {shown}");
            assert!(shown.contains("16:04:05"), "{language:?}: {shown}");
        }
        set_language(Locale::En);
        assert_eq!(display_time("not a date"), "not a date", "an unparseable value shows as it is");
    }

    /// Stock presets and panel headers are translated; a user preset keeps its name, also when it
    /// shares a stock preset's name.
    #[test]
    fn traditional_chinese_presets_and_panel_headers_are_painted() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        crate::panels::presets::reveal(&mut app).unwrap();
        let builtin = app.session.presets.iter().find(|p| p.name == "Warm Glow").unwrap().clone();
        app.session.presets.push(lcc_develop::Preset { id: "user.test".into(), builtin: false, ..builtin });
        let zh = Locale::ZhHant;
        let groups: Vec<String> = app.session.presets.iter().map(|p| format!("preset:{}", p.group)).collect();
        app.ui.group_open.extend(groups.into_iter().map(|g| (g, true)));
        // the presets are a panel of Develop's left column
        let text = painted_text(&ctx, &mut app, zh);
        assert!(text.contains(zh.tr("Warm Glow")), "stock preset: {text}");
        assert!(text.contains("Warm Glow"), "user preset: {text}");
        assert!(text.contains(zh.tr("Color")), "stock group: {text}");
        for (panel, title) in [(crate::state::RightPanel::Activity, "History"), (crate::state::RightPanel::Versions, "Versions")] {
            app.ui.right = panel;
            let text = painted_text(&ctx, &mut app, zh);
            assert!(text.contains(zh.tr(title)), "{text}");
        }
        assert_eq!(history_label("Exposure", &app.session.presets), zh.tr("Exposure"));
        assert!(history_label("Preset: Warm Film", &app.session.presets).ends_with(zh.tr("Warm Film")));
        assert!(history_label("Preset: Warm Glow", &app.session.presets).ends_with("Warm Glow"), "the user's preset shares the name");
        set_language(Locale::En);
    }

    /// The messages each language lacks, which it shows in English. Never fails: `cargo xtask ci`
    /// runs it with `--nocapture`, so the list lands in the gate log. Plain messages are every key
    /// some catalog has plus the labels the UI passes to `tr` from tables (commands, controls, rule
    /// fields…); formats are every key some `*-formats.json` has plus every `tr_format!` message in
    /// the app's source. An empty translation counts as missing.
    #[test]
    fn translation_gaps_report() {
        let notes = whats_new_lines();
        let messages: std::collections::BTreeSet<String> = plain_messages().into_iter().filter(|m| !notes.contains(&m.as_str())).collect();
        let formats = format_messages();
        let quoted = |list: &[&str]| list.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(", ");
        let mut out = format!(
            "Translation gaps, shown in English ({} messages, {} formats, {} What's New lines):\n",
            messages.len(),
            formats.len(),
            notes.len()
        );
        for language in Locale::ALL.iter().filter(|language| **language != Locale::En) {
            let (missing, missing_formats) = gaps(*language, &messages, &formats);
            let missing_notes = notes.iter().filter(|line| !language.catalog().contains_key(**line)).count();
            out.push_str(&format!(
                "{}: {} messages, {} formats, {} What's New lines\n",
                language.code(),
                missing.len(),
                missing_formats.len(),
                missing_notes
            ));
            if !missing.is_empty() {
                out.push_str(&format!("  messages: {}\n", quoted(&missing)));
            }
            if !missing_formats.is_empty() {
                out.push_str(&format!("  formats: {}\n", quoted(&missing_formats)));
            }
        }
        println!("{out}");
    }

    /// The plain messages and formats `language` lacks, out of `messages` and `formats`.
    fn gaps<'a>(
        language: Locale,
        messages: &'a std::collections::BTreeSet<String>,
        formats: &'a std::collections::BTreeSet<String>,
    ) -> (Vec<&'a str>, Vec<&'a str>) {
        let (catalog, own_formats) = (language.catalog(), formats_catalog(language.code()));
        (
            messages.iter().map(String::as_str).filter(|m| !catalog.contains_key(*m)).collect(),
            formats.iter().map(String::as_str).filter(|m| !own_formats.contains_key(*m)).collect(),
        )
    }

    /// No language lacks any of the messages and formats the gaps report knows (Help ▸ What's New's
    /// lines aside, upstream's release notes): a gap would show English text.
    #[test]
    fn no_language_has_translation_gaps() {
        let notes = whats_new_lines();
        let messages: std::collections::BTreeSet<String> = plain_messages().into_iter().filter(|m| !notes.contains(&m.as_str())).collect();
        let formats = format_messages();
        let mut missing = Vec::new();
        for language in Locale::ALL.iter().filter(|language| **language != Locale::En) {
            let (plain, format) = gaps(*language, &messages, &formats);
            missing.extend(plain.iter().map(|m| format!("{} lacks {m:?}", language.code())));
            missing.extend(format.iter().map(|m| format!("{} lacks the format {m:?}", language.code())));
        }
        assert!(missing.is_empty(), "{} translation(s) missing, shown in English:\n{}", missing.len(), missing.join("\n"));
    }

    /// Every plain message: the keys of every catalog (but contextual entries, [`CONTEXTUAL`], which a
    /// language adds only where it needs one) and the labels the UI translates from tables.
    fn plain_messages() -> std::collections::BTreeSet<String> {
        use lcc_catalog::rules;
        let mut labels: Vec<&str> = lcc_engine::command_specs().iter().map(|spec| spec.label).collect();
        labels.extend(crate::menus::ui_commands().map(|command| command.1).filter(|label| !Locale::ALL.iter().any(|locale| locale.name() == *label)));
        labels.extend(lcc_develop::CONTROLS.iter().map(|control| control.label));
        labels.extend(crate::panels::settings::TABS.iter().map(|(_, label)| *label));
        for (_, label, kind) in rules::FIELDS {
            labels.push(label);
            labels.extend(rules::ops_for(*kind).iter().map(|(_, label)| *label));
            if let rules::Kind::Choice(choices) = kind {
                labels.extend(choices.iter().map(|(_, label)| *label));
            }
        }
        labels.extend(rules::FIELD_GROUPS.iter().map(|group| group.0));
        labels.extend(rules::Issue::ALL.iter().map(|issue| issue.text()));
        labels.extend([rules::bool_label(true), rules::bool_label(false)]);
        labels.extend(lcc_engine::rename::TOKENS.iter().map(|token| token.meaning));
        labels.extend(lcc_engine::rename::TEMPLATE_NOTES);
        labels.extend(crate::date_picker::WEEKDAY_SHORT);
        labels.extend(["Pick a date", "Year", "Month", "Day", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]);
        // the album pickers
        labels.extend([
            "Choose a collection…",
            "No collections yet",
            "Search collections",
            "No collections match",
            "This is the collection you're editing",
            "It tests this collection, so testing it back would loop",
            "Fix the marked rules to save this album.",
            "None",
            "New collection",
            "New collection…",
            "Imported Photos",
        ]);
        labels.extend(crate::panels::keyword_list::VARIABLE_LABELS);
        // and every literal the source hands to `tr` or a helper that translates (`tr_format!`
        // messages are the formats' list); a place that asks with a context needs the plain message
        let formats = format_messages();
        let scanned = crate::i18n_scan::translated_texts()
            .into_keys()
            .filter(|text| !formats.contains(text))
            .map(|text| contextual(&text).map_or(text.clone(), |(_, plain)| plain.to_string()));
        Locale::ALL
            .iter()
            .flat_map(|language| language.catalog().keys())
            .filter(|key| contextual(key).is_none())
            .cloned()
            .chain(labels.into_iter().map(str::to_string))
            .chain(scanned)
            .collect()
    }

    /// Help ▸ What's New, line by line as the dialog looks each one up.
    fn whats_new_lines() -> Vec<&'static str> {
        crate::panels::dialogs::WHATS_NEW
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("# "))
            .map(|line| line.strip_prefix("### ").or_else(|| line.strip_prefix("## ")).or_else(|| line.strip_prefix("- ")).unwrap_or(line))
            .collect()
    }

    /// `locales/<code>-formats.json` with the fork's rows (`locales/fork/`) over it, as `build.rs`
    /// compiles them into `tr_format!`, without empty translations.
    fn formats_catalog(code: &str) -> BTreeMap<String, String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let read = |path: std::path::PathBuf| -> BTreeMap<String, String> {
            std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
        };
        let mut messages = read(dir.join(format!("{code}-formats.json")));
        messages.extend(read(dir.join("fork").join(format!("{code}-formats.json"))));
        messages.retain(|_, text| !text.is_empty());
        messages
    }

    /// Every format message: the keys of every formats catalog and the message of every
    /// `tr_format!` call in the app's source (`i18n_scan::sources`: no test files or modules).
    fn format_messages() -> std::collections::BTreeSet<String> {
        let mut all: std::collections::BTreeSet<String> = Locale::ALL
            .iter()
            .filter(|language| **language != Locale::En)
            .flat_map(|language| formats_catalog(language.code()).into_keys())
            .collect();
        for source in crate::i18n_scan::sources() {
            all.extend(tr_format_messages(&source.code));
        }
        all
    }

    /// The first argument of each `tr_format!` call in `source`, when it is a string literal.
    fn tr_format_messages(source: &str) -> Vec<String> {
        // built from parts, so this file's own source holds no call to find
        let call = concat!("tr_format", "!(");
        let mut messages = Vec::new();
        for (at, _) in source.match_indices(call) {
            let Some(body) = source[at + call.len()..].trim_start().strip_prefix('"') else { continue };
            let mut message = String::new();
            let mut chars = body.chars();
            while let Some(ch) = chars.next() {
                match ch {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => message.push('\n'),
                        Some('t') => message.push('\t'),
                        Some('u') => {
                            let hex: String = chars.by_ref().skip(1).take_while(|ch| *ch != '}').collect();
                            message.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                        }
                        Some(other) => message.push(other),
                        None => break,
                    },
                    _ => message.push(ch),
                }
            }
            messages.push(message);
        }
        messages
    }

    #[test]
    fn tr_format_messages_are_read_from_source() {
        let call = concat!("tr_format", "!(");
        let source = format!("{call}\"Imported {{}} photo{{}}\", n, s); crate::i18n::{call}\n    \"Say \\\"hi\\\" \\u{{2192}}\", x); {call}name)");
        assert_eq!(tr_format_messages(&source), ["Imported {} photo{}", "Say \"hi\" →"]);
    }

    /// A drop's notice comes from what `library.import` reported: a photo added, one brought back from Recently
    /// Deleted and a file that could not be read are each named; a drop of duplicates only says nothing.
    #[test]
    fn a_dropped_photos_notice_names_every_outcome_of_the_import_and_is_silent_for_duplicates_only() {
        set_language(Locale::En);
        let dir = lcc_testkit::TempDir::new("lc-drop-notice");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let png = |name: &str, seed: u8| {
            let data: Vec<[u8; 4]> = (0..48 * 32).map(|i| [(i % 48 * 5) as u8, (i / 48 * 7) as u8, seed, 255]).collect();
            let img = lcc_raster::Rgba8 { width: 48, height: 32, data };
            let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(dir.join(name), bytes).unwrap();
            dir.join(name).to_string_lossy().into_owned()
        };
        let (trashed, added) = (png("trashed.png", 1), png("new.png", 2));
        let broken = dir.join("broken.jpg");
        std::fs::write(&broken, b"not a jpeg at all").unwrap();
        let mut s = lcc_engine::Session::new().with_fs();
        let r = s.execute("library.import", &serde_json::json!({"paths": [trashed]})).unwrap();
        s.execute("photo.delete", &serde_json::json!({"ids": [r["imported"][0]]})).unwrap();
        let report = s.execute("library.import", &serde_json::json!({"paths": [trashed, added, broken]})).unwrap();
        assert_eq!(
            dropped_photos_outcome(&report).as_deref(),
            Some("Added 1 photo · Restored 1 photo from Recently Deleted · 1 photo not readable"),
            "{report}"
        );
        let again = s.execute("library.import", &serde_json::json!({"paths": [added]})).unwrap();
        assert_eq!(dropped_photos_outcome(&again), None, "a duplicate only: {again}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Polish: the menu and control channel select it, dates read day.month.year, and every count
    /// reads right for any number. Polish has three plural forms (1 zdjęcie, 2 zdjęcia, 5 zdjęć), so
    /// counts follow their noun ("Zaimportowane zdjęcia: 22") instead of a form that fits only some.
    #[test]
    fn polish_switches_formats_dates_and_reads_every_count() {
        use crate::control::{ControlRequest, Outcome};
        let mut app = crate::LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        app.run("app.language.polish", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Locale::Pl);
        assert_eq!(Locale::Pl.name(), "Polski");
        assert_eq!(crate::menubar::checked(&app, "app.language.polish"), Some(true));
        assert_eq!(tr("File"), "Plik");
        assert_eq!(tr("Exposure"), "Ekspozycja");
        assert_eq!(date_group_label("2026-09-20", false), "Niedziela, 20.09.2026");
        assert_eq!(date_group_label("2026-09-20", true), "Niedziela, 20");
        assert_eq!(date_group_label("2026-09", false), "09.2026");
        assert_eq!(date_group_label("2026", false), "2026");
        assert_eq!(display_time("2026-09-20T16:04:05"), "Niedziela, 20.09.2026 16:04:05");
        for n in [0, 1, 2, 3, 4, 5, 12, 21, 22, 25, 111] {
            let s = if n == 1 { "" } else { "s" };
            assert_eq!(tr_format!("Imported {} photo{}", n, s), format!("Zaimportowane zdjęcia: {n}"));
            for (one, many, noun) in [
                ("photo waiting", "photos waiting", "Oczekujące zdjęcia"),
                ("cached picture", "cached pictures", "Obrazy w pamięci podręcznej"),
                ("photo has its denoised picture.", "photos have their denoised picture.", "Zdjęcia z odszumioną wersją"),
            ] {
                assert_eq!(tr_format!("{n} {noun}", n = n, noun = tr(if n == 1 { one } else { many })), format!("{noun}: {n}"));
            }
            assert_eq!(tr_format!("Delete {n} Photos", n = n), format!("Usuń zdjęcia ({n})"));
            assert_eq!(tr_format!("{n} preset{}", s, n = n), format!("Ustawienia: {n}"));
        }
        let name = String::from("Color 1234.56 {title} 日本語");
        assert_eq!(tr_format!("Added {n} photo{} to “{}”", "s", name, n = 22), format!("Dodano do „{name}”: 22"));
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, Locale::Pl);
        let ctx = egui::Context::default();
        for tag in ["pl", "pl-PL", "pl_PL.UTF-8", "PL-pl"] {
            assert_eq!(Locale::parse_tag(tag), Some(Locale::Pl), "{tag}");
            let (request, _) = ControlRequest::new("ui.set", serde_json::json!({"language": tag}));
            let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &request) else { panic!("expected reply") };
            assert_eq!(reply["ok"], true);
            assert_eq!(language(), Locale::Pl);
        }
        let text = painted_text(&ctx, &mut app, Locale::Pl);
        assert!(text.contains("Plik") && text.contains("Ekspozycja"), "{text}");
        ctx.fonts_mut(|fonts| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                let font = egui::FontId::new(13.0, family);
                for ch in "ąćęłńóśźżĄĆĘŁŃÓŚŹŻ„”".chars() {
                    assert!(fonts.has_glyph(&font, ch), "missing Polish glyph {ch}");
                }
            }
        });
        set_language(Locale::En);
    }
}

#[cfg(test)]
mod picker_order_tests {
    use super::*;

    #[test]
    fn picker_lists_english_first_then_names_alphabetically() {
        let order = Locale::for_picker();
        assert_eq!(order.first(), Some(&Locale::En));
        assert_eq!(order.len(), Locale::ALL.len());
        let keys: Vec<String> = order[1..].iter().map(|l| name_sort_key(l.name())).collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{keys:?}");
        assert_eq!(name_sort_key("Čeština"), "cestina");
    }

    #[test]
    fn language_menu_lists_the_picker_order() {
        let menu: Vec<&str> = crate::menus::LANGUAGE_COMMANDS.iter().map(|c| c.1).collect();
        let picker: Vec<&str> = Locale::for_picker().into_iter().map(Locale::name).collect();
        assert_eq!(menu, picker, "Edit ▸ Language and the Settings row list the languages the same way");
    }
}
