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
