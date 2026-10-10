//! P6.3 localisation coverage: every catalog carries the same messages (a message missing from one
//! language fails, it doesn't quietly show English), and every text the fork's own UI passes to
//! `tr` (directly, or through a helper that translates) is in every catalog.

use std::collections::{BTreeMap, BTreeSet};

use crate::i18n::Locale;

fn catalog(code: &str, formats: bool) -> BTreeMap<String, String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let name = if formats { format!("{code}-formats.json") } else { format!("{code}.json") };
    let text = std::fs::read_to_string(dir.join(&name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut messages: BTreeMap<String, String> = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    // the fork's rows (`locales/fork/`), merged over upstream's as the app does
    if let Ok(text) = std::fs::read_to_string(dir.join("fork").join(&name)) {
        messages.extend(serde_json::from_str::<BTreeMap<String, String>>(&text).unwrap_or_else(|e| panic!("fork/{name}: {e}")));
    }
    messages
}

fn translated() -> impl Iterator<Item = Locale> {
    Locale::ALL.iter().copied().filter(|l| *l != Locale::En)
}

#[test]
fn every_catalog_carries_every_message() {
    for formats in [false, true] {
        let all: Vec<(Locale, BTreeMap<String, String>)> = translated().map(|l| (l, catalog(l.code(), formats))).collect();
        let union: BTreeSet<&String> = all.iter().flat_map(|(_, c)| c.keys()).collect();
        let mut missing = Vec::new();
        for (l, c) in &all {
            for key in &union {
                if !c.contains_key(*key) {
                    missing.push(format!("{} lacks {key:?}", l.code()));
                }
            }
        }
        assert!(missing.is_empty(), "{} message(s) missing (formats: {formats}):\n{}", missing.len(), missing.join("\n"));
    }
}

/// The fork's UI files (owned in `upstream-owned.txt`) whose text must be translated.
const OWNED: &[(&str, &str)] = &[
    ("map/mod.rs", include_str!("map/mod.rs")),
    ("map/side.rs", include_str!("map/side.rs")),
    ("map/view.rs", include_str!("map/view.rs")),
    ("book.rs", include_str!("book.rs")),
    ("slideshow_ui.rs", include_str!("slideshow_ui.rs")),
    ("print_ui.rs", include_str!("print_ui.rs")),
    ("web_module.rs", include_str!("web_module.rs")),
    ("creations_ui.rs", include_str!("creations_ui.rs")),
    ("edit_in.rs", include_str!("edit_in.rs")),
    ("module.rs", include_str!("module.rs")),
    ("help_overlay.rs", include_str!("help_overlay.rs")),
    ("access.rs", include_str!("access.rs")),
    ("panels/publish.rs", include_str!("panels/publish.rs")),
    ("panels/tether_bar.rs", include_str!("panels/tether_bar.rs")),
    ("panels/connections.rs", include_str!("panels/connections.rs")),
    ("panels/plugins.rs", include_str!("panels/plugins.rs")),
    ("panels/classic.rs", include_str!("panels/classic.rs")),
    ("panels/navigator.rs", include_str!("panels/navigator.rs")),
    ("panels/metadata.rs", include_str!("panels/metadata.rs")),
    ("panels/cells.rs", include_str!("panels/cells.rs")),
];

/// Calls whose first string literal is translated: `tr("…")`, and the helpers that call `tr` on
/// their text argument.
const TRANSLATING: &[&str] =
    &["tr(", "tr_format!(", "access::button(", "access::choice(", "access::label(", "section(ui, ", "heading(ui, ", "small(ui, "];

/// The Rust string literal starting at `s` (just after its opening quote), unescaped; `None` for
/// anything this little reader doesn't understand (it is skipped, not guessed).
fn literal(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '\'' => out.push('\''),
                '\n' => {
                    // a line continuation: the next line's leading spaces are dropped
                    let rest = chars.as_str().trim_start();
                    chars = rest.chars();
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// Every literal the owned files hand to a translating call (test modules excluded).
fn owned_texts() -> BTreeMap<String, &'static str> {
    let mut found = BTreeMap::new();
    for (file, src) in OWNED {
        let code = src.split("#[cfg(test)]\nmod tests {").next().unwrap_or(src);
        for call in TRANSLATING {
            let mut rest = code;
            while let Some(i) = rest.find(call) {
                let before = &rest[..i];
                rest = &rest[i + call.len()..];
                // `tr(` must be the function, not the end of another name (`attr(`, `str(`)
                if *call == "tr(" && before.chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                // the first argument for tr / tr_format; for access::* and helpers, the first literal
                let args = rest.trim_start();
                let start = match *call {
                    "tr(" | "tr_format!(" => args.strip_prefix('"'),
                    _ => args
                        .split_once(['"', ')', ';'])
                        .and_then(|(head, _)| (!head.contains('(')).then(|| &args[head.len()..]))
                        .and_then(|a| a.strip_prefix('"')),
                };
                let Some(text) = start.and_then(literal) else { continue };
                if text.chars().any(char::is_alphabetic) {
                    found.entry(text).or_insert(*file);
                }
            }
        }
    }
    found
}

#[test]
fn owned_ui_text_is_in_every_catalog() {
    let texts = owned_texts();
    assert!(texts.len() > 300, "the scan finds the owned UI's text ({} messages)", texts.len());
    assert!(texts.contains_key("Map Style") && texts.contains_key("Zoom In"), "helpers and access names are scanned");
    let mut missing = Vec::new();
    for l in translated() {
        let (plain, formats) = (catalog(l.code(), false), catalog(l.code(), true));
        for (text, file) in &texts {
            if !plain.contains_key(text) && !formats.contains_key(text) {
                missing.push(format!("{}: {text:?} ({file})", l.code()));
            }
        }
    }
    assert!(missing.is_empty(), "{} untranslated message(s):\n{}", missing.len(), missing.join("\n"));
}
