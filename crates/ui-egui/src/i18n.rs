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
                        Ok(mut messages) => {
                            messages.retain(|_, text| !text.is_empty());
                            messages
                        }
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
    Es, "es", "Español", "Latn", include_str!("../locales/es.json");
    De, "de", "Deutsch", "Latn", include_str!("../locales/de.json");
    Ru, "ru", "Русский", "Cyrl", include_str!("../locales/ru.json");
    Fr, "fr", "Français", "Latn", include_str!("../locales/fr.json");
    Uk, "uk", "Українська", "Cyrl", include_str!("../locales/uk.json");
    Pl, "pl", "Polski", "Latn", include_str!("../locales/pl.json");
    Cs, "cs", "Čeština", "Latn", include_str!("../locales/cs.json");
    It, "it", "Italiano", "Latn", include_str!("../locales/it.json");
    Ka, "ka", "ქართული", "Geor", include_str!("../locales/ka.json");
    Lo, "lo", "ພາສາລາວ", "Laoo", include_str!("../locales/lo.json");
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

/// The UI language "Auto" resolves to: the environment's (`LIGHTCRAFT_LANGUAGE=zh-hans`, for
/// headless runs), or the host-installed default ([`set_default_language`], the browser's languages
/// on the web), else English.
pub fn default_language() -> Locale {
    std::env::var("LIGHTCRAFT_LANGUAGE")
        .ok()
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

/// [`tr`] for a message whose translation depends on where it appears: a catalog entry keyed
/// `"<context>|<source>"` wins, else the plain message. A language adds the contextual entry only
/// where its shared translation reads wrongly there (Japanese はい / いいえ as a rule's value).
fn tr_ctx_in<'a>(language: Locale, context: &str, source: &'a str) -> &'a str {
    match language.catalog().get(&format!("{context}|{source}")) {
        Some(translation) => translation.as_str(),
        None => tr_in(language, source),
    }
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

/// `"s"` for a count other than one: the plural ending of English messages. Computed before a [`tr_format!`] rather
/// than inside it, since the macro repeats its arguments once per language.
pub(crate) fn plural_s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
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
        Some(path) => tr_format!("LightCraft could not open its window: {e}\n\nThe log file has the details: {path}", e = error, path = path),
        None => tr_format!("LightCraft could not open its window: {e}", e = error),
    };
    (tr("LightCraft could not start").to_string(), text)
}

/// The title and text of the message box shown when LightCraft stopped while its window was up
/// (issue #1337: the window's renderer after the graphics driver reset the GPU), in the UI
/// language: what happened and, when there is one, the log file to look in.
pub fn stopped_message(error: &str, log_file: Option<&str>) -> (String, String) {
    let text = match log_file {
        Some(path) => tr_format!("LightCraft stopped unexpectedly: {e}\n\nThe log file has the details: {path}", e = error, path = path),
        None => tr_format!("LightCraft stopped unexpectedly: {e}", e = error),
    };
    (tr("LightCraft stopped").to_string(), text)
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
    builtin_label(name, lightcraft_engine::presets::profile(id).is_some())
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

/// What the grid is titled for the session's source: [`source_label`], or a library folder's last
/// two names (`photos/travel`; the folder's name is the user's and stays verbatim).
pub fn source_title(session: &lightcraft_engine::Session) -> String {
    match (session.source, session.library_folder.as_deref()) {
        (lightcraft_engine::LibrarySource::LibraryFolder, Some(path)) => lightcraft_catalog::folders::folder_label(path),
        (source, _) => source_label(source, &session.catalog),
    }
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

/// Built-in colour names are translated; custom label names are user data.
pub fn color_label(catalog: &lightcraft_catalog::Catalog, label: lightcraft_catalog::ColorLabel) -> String {
    catalog.custom_label_name(label).map_or_else(|| tr(&catalog.label_name(label)).to_string(), str::to_string)
}

/// A legacy smart-album filter summary, using the same display labels as filter chips.
pub fn filter_label(filter: &lightcraft_catalog::Filter, catalog: &lightcraft_catalog::Catalog) -> String {
    if language() == Locale::En {
        filter.describe_with(catalog)
    } else {
        lightcraft_engine::filter_chips(filter, catalog)
            .iter()
            .map(|chip| crate::panels::chips::display_label(chip, filter, catalog))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// What people read for a smart-album rule's problem, under its row: the field's name and the
/// issue, in the current language ("Title: needs something to look for").
pub fn problem_text(problem: &lightcraft_catalog::rules::Problem) -> String {
    problem_text_in(language(), problem)
}

/// [`problem_text`] with the rule's position first ("#2.1 Rating: …"), for lists away from the
/// editor (the sidebar's tooltip).
pub fn problem_line(problem: &lightcraft_catalog::rules::Problem) -> String {
    problem_line_in(language(), problem)
}

fn problem_text_in(language: Locale, problem: &lightcraft_catalog::rules::Problem) -> String {
    let issue = tr_in(language, problem.issue.text());
    // Japanese and Chinese catalogs join a label and its text with a full-width colon
    let colon = if matches!(language.script(), "Jpan" | "Hans" | "Hant") { "：" } else { ": " };
    match problem.field.as_deref() {
        Some(field) => format!("{}{colon}{issue}", tr_in(language, lightcraft_catalog::rules::field_label(field).unwrap_or(field))),
        None => issue.to_string(),
    }
}

fn problem_line_in(language: Locale, problem: &lightcraft_catalog::rules::Problem) -> String {
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
    lightcraft_catalog::rules::choice_label(field, id).map_or(id, |label| tr_in(language, label))
}

/// What people read for a smart-album yes/no value ("Ja" / "Nein" in German).
pub fn bool_text(b: bool) -> &'static str {
    bool_text_in(language(), b)
}

/// The context of a smart-album yes/no value ([`tr_ctx_in`]).
const BOOL_CONTEXT: &str = "smart album value";

fn bool_text_in(language: Locale, b: bool) -> &'static str {
    tr_ctx_in(language, BOOL_CONTEXT, lightcraft_catalog::rules::bool_label(b))
}

/// A rule summary for display, keeping free-text rule values verbatim.
pub fn rules_label(rules: &lightcraft_catalog::RuleSet, catalog: &lightcraft_catalog::Catalog) -> String {
    fn describe(rules: &lightcraft_catalog::RuleSet, catalog: &lightcraft_catalog::Catalog, depth: usize) -> String {
        use lightcraft_catalog::{
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
                        Kind::Album => lightcraft_catalog::rules::album_name(value, Some(catalog)),
                        Kind::Choice(_) => value.as_str().map_or_else(|| value.to_string(), |id| choice_text(field, id).to_string()),
                        Kind::Bool => lightcraft_catalog::rules::bool_value(value).map_or_else(|| value.to_string(), |b| bool_text(b).to_string()),
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

    #[test]
    fn czech_coverage_of_catalogs_commands_controls_and_rules_is_reported() {
        let catalog = Locale::Cs.catalog();
        let keys: std::collections::BTreeSet<&String> = Locale::ALL.iter().flat_map(|language| language.catalog().keys()).collect();
        let lacking: Vec<_> = keys.into_iter().filter(|key| !catalog.contains_key(key.as_str())).collect();
        if !lacking.is_empty() {
            eprintln!("cs lacks {} catalog message(s) (shown in English): {lacking:?}", lacking.len());
        }
        let mut labels: Vec<&str> = lightcraft_engine::command_specs().iter().map(|spec| spec.label).collect();
        labels.extend(crate::menus::ui_commands().map(|command| command.1).filter(|label| !Locale::ALL.iter().any(|locale| locale.name() == *label)));
        labels.extend(lightcraft_develop::CONTROLS.iter().map(|control| control.label));
        labels.extend(crate::panels::settings::TABS.iter().map(|(_, label)| *label));
        labels.extend(lightcraft_catalog::rules::FIELD_GROUPS.iter().map(|group| group.0));
        labels.extend(lightcraft_engine::rename::TOKENS.iter().map(|token| token.meaning));
        labels.extend(lightcraft_engine::rename::TEMPLATE_NOTES);
        let missing: Vec<_> = labels.into_iter().filter(|label| !catalog.contains_key(*label)).collect();
        if !missing.is_empty() {
            eprintln!("cs lacks {} display label(s) (shown in English): {missing:?}", missing.len());
        }
    }

    #[test]
    fn czech_switches_persists_and_formats_without_changing_user_values() {
        use crate::control::{ControlRequest, Outcome};
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        app.run("app.language.czech", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Locale::Cs);
        assert_eq!(Locale::Cs.name(), "Čeština");
        assert_eq!(Locale::Cs.script(), "Latn");
        assert_eq!(crate::menubar::checked(&app, "app.language.czech"), Some(true));
        assert_eq!(tr("File"), "Soubor");
        assert_eq!(tr("Develop"), "Vyvolat");
        assert_eq!(date_group_label("2026-09-20", false), "Neděle, 20. 9. 2026");
        assert_eq!(date_group_label("2026-09-20", true), "Neděle, 20.");
        assert_eq!(date_group_label("2026-09", false), "9. 2026");
        assert_eq!(date_group_label("2026", false), "2026");
        assert_eq!(display_time("2026-09-20T16:04:05"), "Neděle, 20. 9. 2026 16:04:05");
        for n in [0, 1, 2, 4, 5, 11, 21, 22, 25] {
            assert_eq!(tr_format!("Imported {} photo{}", n, if n == 1 { "" } else { "s" }), format!("Importováno fotografií: {n}"));
            assert_eq!(
                tr_format!("{n} {noun}", n = n, noun = tr(if n == 1 { "photo waiting" } else { "photos waiting" })),
                format!("Čekající fotografie: {n}")
            );
        }
        let name = String::from("Color 1234.56 {title} 日本語");
        assert_eq!(tr_format!("Added {n} photo{} to “{}”", "s", name, n = 22), format!("Do „{name}“ přidáno fotografií: 22"));
        assert_eq!(builtin_label("Warm Glow", false), "Warm Glow");
        assert_eq!(tr("photo-{title}.jpg"), "photo-{title}.jpg");
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, Locale::Cs);
        let ctx = egui::Context::default();
        for tag in ["cs", "cs-CZ", "cs_CZ.UTF-8", "cs-Latn-CZ"] {
            assert_eq!(Locale::parse_tag(tag), Some(Locale::Cs));
            let (request, _) = ControlRequest::new("ui.set", serde_json::json!({"language": tag}));
            let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &request) else { panic!("expected reply") };
            assert_eq!(reply["ok"], true);
            assert_eq!(language(), Locale::Cs);
        }
        let text = painted_text(&ctx, &mut app, Locale::Cs);
        assert!(text.contains("Soubor") && text.contains("Expozice"), "{text}");
        ctx.fonts_mut(|fonts| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                let font = egui::FontId::new(13.0, family);
                for ch in "áčďéěíňóřšťúůýžÁČĎÉĚÍŇÓŘŠŤÚŮÝŽ„“".chars() {
                    assert!(fonts.has_glyph(&font, ch), "missing Czech glyph {ch}");
                }
            }
        });
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
    /// The message box for a desktop start that failed (issue #260) names the error and the log
    /// file, in the UI language.
    #[test]
    fn startup_failure_message_names_the_error_and_the_log() {
        let (title, text) = startup_failed_message("no adapter", Some("/home/a/.config/lightcraft/logs/lightcraft.log"));
        assert_eq!(title, "LightCraft could not start");
        assert!(text.contains("no adapter") && text.ends_with("lightcraft.log"), "{text}");
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
        assert_eq!(Locale::parse_tag("xx"), None);
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
        set_language(Locale::Fr);
        assert_eq!(tr_format!("Imported {} photo{}", 12, "s"), "12 photos importée(s)");
        assert_eq!(tr_format!("Imported {} photo{}", 1, ""), "1 photo importée(s)");
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
                lightcraft_testkit::skip!(Fonts, "{}: built without a craft-fonts face for {}", language.code(), language.script());
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

    #[test]
    fn translated_catalogs_do_not_use_missing_menu_triangles() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let messages: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for (source, text) in messages {
                assert!(!text.contains(['▸', '▾']), "{}: {source:?} uses an unpaintable menu triangle", path.display());
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
            ("app.language.lao", Locale::Lo),
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

    /// Smart-album choice values read as words in every language (English "Public Domain", German
    /// "Gemeinfrei"), never as rule ids like `publicDomain`; an id the field doesn't know shows as
    /// written. Unlike other labels these must be translated: an untranslated one would be English
    /// in the middle of a translated rule.
    #[test]
    fn choice_values_read_as_words_in_every_language() {
        use lightcraft_catalog::rules::{FIELDS, Kind};
        assert_eq!(choice_text_in(Locale::En, "copyrightStatus", "publicDomain"), "Public Domain");
        assert_eq!(choice_text_in(Locale::De, "copyrightStatus", "publicDomain"), "Gemeinfrei");
        assert_eq!(choice_text_in(Locale::De, "copyrightStatus", "someday"), "someday");
        assert_eq!(choice_text_in(Locale::ZhHans, "treatment", "color"), "彩色", "in colour, not the colour noun");
        assert_eq!(choice_text_in(Locale::Ru, "treatment", "color"), "Цветное");
        assert_eq!(choice_text_in(Locale::Es, "label", "none"), "Sin etiqueta");
        // yes/no fields read Yes / No, never true / false
        assert_eq!((bool_text_in(Locale::En, true), bool_text_in(Locale::En, false)), ("Yes", "No"));
        assert_eq!(bool_text_in(Locale::De, false), "Nein");
        // Japanese answers はい / いいえ don't read as a rule's value: its own wording, あり / なし,
        // through a context, while the shared Yes / No keep their meaning everywhere else
        assert_eq!((bool_text_in(Locale::Ja, true), bool_text_in(Locale::Ja, false)), ("あり", "なし"));
        assert_eq!((Locale::Ja.tr("Yes"), Locale::Ja.tr("No")), ("はい", "いいえ"));
        assert_eq!(tr_ctx_in(Locale::Ja, "nope", "Yes"), "はい", "no contextual entry: the plain message");
        assert_eq!(tr_ctx_in(Locale::En, BOOL_CONTEXT, "Yes"), "Yes");
        // and the yes/no fields are named by plain nouns, so "編集 … なし" doesn't read "edits: yes … none"
        for (field, label, kind) in FIELDS {
            if *kind != Kind::Bool {
                continue;
            }
            let ja = Locale::Ja.catalog().get(*label).unwrap_or_else(|| panic!("ja lacks the {field} label {label:?}"));
            assert!(!ja.contains("あり") && !ja.contains("なし"), "{field}: {ja}");
        }
        // a contextual entry names a real value (a typo would silently fall back to はい / いいえ)
        let values = [lightcraft_catalog::rules::bool_label(true), lightcraft_catalog::rules::bool_label(false)];
        for language in Locale::ALL {
            for key in language.catalog().keys() {
                if let Some(value) = key.strip_prefix(BOOL_CONTEXT).and_then(|k| k.strip_prefix('|')) {
                    assert!(values.contains(&value), "{}: {key:?} isn't a yes/no value", language.code());
                }
            }
        }
        for language in Locale::ALL.iter().filter(|language| **language != Locale::En) {
            for label in [lightcraft_catalog::rules::bool_label(true), lightcraft_catalog::rules::bool_label(false)] {
                assert!(language.catalog().contains_key(label), "{} lacks {label:?}", language.code());
            }
        }
        for language in Locale::ALL {
            for (field, _, kind) in FIELDS {
                let Kind::Choice(choices) = kind else { continue };
                for (id, label) in *choices {
                    let text = choice_text_in(*language, field, id);
                    assert_ne!(text, *id, "{}: {field} {id}", language.code());
                    if *language != Locale::En {
                        assert!(language.catalog().contains_key(*label), "{} lacks {label:?}", language.code());
                    }
                }
            }
        }
    }

    /// A rule's problem reads as the field's name and the issue, in the editor's language ("Titel:
    /// braucht einen Suchbegriff …"); the sidebar's list puts the rule's position first. Every issue
    /// is translated in every language: an English one would stand out in a translated editor.
    #[test]
    fn problems_read_in_every_language() {
        use lightcraft_catalog::rules::Issue;
        let cat = lightcraft_catalog::Catalog::new();
        let problems = |rules: serde_json::Value| {
            serde_json::from_value::<lightcraft_catalog::RuleSet>(serde_json::json!({"rules": rules})).unwrap().check(&cat)
        };
        let p = problems(serde_json::json!([{"field": "rating", "op": "gte", "value": 3}, {"field": "title", "op": "contains", "value": ""}]));
        assert_eq!(problem_text_in(Locale::En, &p[0]), "Title: needs something to look for (or use “is empty”)");
        assert_eq!(problem_text_in(Locale::De, &p[0]), "Titel: braucht einen Suchbegriff (oder „ist leer“)");
        assert_eq!(problem_line_in(Locale::En, &p[0]), "#2 Title: needs something to look for (or use “is empty”)");
        // Japanese and Chinese join with a full-width colon, as their catalogs do
        assert_eq!(problem_text_in(Locale::Ja, &p[0]), "タイトル：検索する語句が必要です（または「が空」を使用）");
        // a problem with the album filter itself has no rule position to show
        let filter_loop =
            lightcraft_catalog::rules::Problem { path: Vec::new(), field: Some("album".into()), issue: Issue::AlbumLoop, message: String::new() };
        assert!(problem_line_in(Locale::En, &filter_loop).starts_with("Album: "), "{}", problem_line_in(Locale::En, &filter_loop));
        let g = problems(serde_json::json!([{"group": {"rules": []}}]));
        assert_eq!(problem_text_in(Locale::En, &g[0]), "This group is empty: add a rule or remove it.");
        // the issues, the operators their hints name and the album picker's words, in every language
        for language in Locale::ALL.iter().filter(|l| **l != Locale::En) {
            // the field menu: every field and every group, so it never mixes languages
            for label in lightcraft_catalog::rules::FIELDS.iter().map(|f| f.1).chain(lightcraft_catalog::rules::FIELD_GROUPS.iter().map(|g| g.0)) {
                assert!(language.catalog().contains_key(label), "{} lacks the rule label {label:?}", language.code());
            }
            for word in [
                "Choose an album…",
                "No albums yet",
                "Search albums",
                "No albums match",
                "This is the album you're editing",
                "It tests this album, so testing it back would loop",
                "Fix the marked rules to save this album.",
            ] {
                assert!(language.catalog().contains_key(word), "{} lacks {word:?}", language.code());
            }
            for issue in Issue::ALL {
                assert!(language.catalog().contains_key(issue.text()), "{} lacks {:?}", language.code(), issue.text());
            }
            for (_, _, kind) in lightcraft_catalog::rules::FIELDS {
                for (_, op) in lightcraft_catalog::rules::ops_for(*kind) {
                    assert!(language.catalog().contains_key(*op), "{} lacks the operator {op:?}", language.code());
                }
            }
        }
    }
mod tests;

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
