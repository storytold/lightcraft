//! `cargo xtask i18n`: keeps the fork's own message catalogs (`crates/ui-egui/locales/fork/*.json`) sorted by
//! key, so two PRs that add strings conflict only when their keys sort next to each other (appending puts every
//! new key after the same last line, and JSON's missing trailing comma changes that line too).
//!
//! - `--check` (a `cargo xtask ci` step): every fork catalog is a JSON object of strings with no repeated key,
//!   its keys in Unicode code-point order (what `BTreeMap` and Python's `sorted` give), in the catalog format:
//!   two-space indent, UTF-8 with no `\u` escapes beyond what JSON requires, a newline at the end.
//! - `--sort`: rewrites the fork catalogs in that order and format. It never adds, drops or changes a key or a
//!   value, and writes nothing when any catalog is broken.
//!
//! Upstream's catalogs (`crates/ui-egui/locales/*.json`) are left as they are, so upstream catalog changes
//! ported by hand still apply where upstream put them.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, Deserializer, MapAccess, Visitor};

const FORK_DIR: &str = "crates/ui-egui/locales/fork";

const USAGE: &str = "usage: cargo xtask i18n --check | --sort";

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    match args {
        ["--check"] => check(root),
        ["--sort"] => sort(root),
        _ => Err(USAGE.to_string()),
    }
}

/// A catalog's entries in file order.
type Entries = Vec<(String, String)>;

/// Reads a catalog's entries in file order: a JSON object of strings, each key once.
fn parse(text: &str) -> Result<Entries, String> {
    struct CatalogVisitor;

    impl<'de> Visitor<'de> for CatalogVisitor {
        type Value = Entries;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a JSON object of strings")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
            let mut seen = HashSet::new();
            let mut entries = Vec::new();
            while let Some(key) = map.next_key::<String>()? {
                if !seen.insert(key.clone()) {
                    return Err(de::Error::custom(format!("the key {key:?} appears twice")));
                }
                let value: serde_json::Value = map.next_value()?;
                let serde_json::Value::String(value) = value else {
                    return Err(de::Error::custom(format!("the value of {key:?} is not a string")));
                };
                entries.push((key, value));
            }
            Ok(entries)
        }
    }

    let mut de = serde_json::Deserializer::from_str(text);
    let entries = (&mut de).deserialize_map(CatalogVisitor).map_err(|e| e.to_string())?;
    de.end().map_err(|e| e.to_string())?;
    Ok(entries)
}

/// The catalog file for `entries`, in their order.
fn render(entries: &[(String, String)]) -> Result<String, String> {
    if entries.is_empty() {
        return Ok("{}\n".to_string());
    }
    let mut lines = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let key = serde_json::to_string(key).map_err(|e| e.to_string())?;
        let value = serde_json::to_string(value).map_err(|e| e.to_string())?;
        lines.push(format!("  {key}: {value}"));
    }
    Ok(format!("{{\n{}\n}}\n", lines.join(",\n")))
}

/// The first key that sorts before the key above it, with that key.
fn first_out_of_order(entries: &[(String, String)]) -> Option<(&str, &str)> {
    entries.windows(2).find_map(|pair| match pair {
        [(above, _), (key, _)] if key < above => Some((key.as_str(), above.as_str())),
        _ => None,
    })
}

/// The fork catalogs, by file name.
fn catalogs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = root.join(FORK_DIR);
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{FORK_DIR}: {e}"))? {
        let path = entry.map_err(|e| format!("{FORK_DIR}: {e}"))?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// `crates/ui-egui/locales/fork/<name>`, for messages.
fn shown(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    format!("{FORK_DIR}/{name}")
}

/// A catalog's text (line endings as git writes them, whatever the checkout did) and its entries.
fn read(path: &Path) -> Result<(String, Entries), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", shown(path)))?;
    let text = text.replace("\r\n", "\n");
    let entries = parse(&text).map_err(|e| format!("{}: {e}", shown(path)))?;
    Ok((text, entries))
}

pub fn check(root: &Path) -> Result<(), String> {
    let paths = catalogs(root)?;
    let mut problems = String::new();
    for path in &paths {
        let (text, mut entries) = match read(path) {
            Ok(read) => read,
            Err(e) => {
                problems.push_str(&format!("\n  {e}"));
                continue;
            }
        };
        if let Some((key, above)) = first_out_of_order(&entries) {
            problems.push_str(&format!("\n  {}: {key:?} is out of order (it sorts before {above:?})", shown(path)));
            continue;
        }
        entries.sort();
        if render(&entries)? != text {
            problems
                .push_str(&format!("\n  {}: in order, but not in the catalog format (two-space indent, UTF-8, a newline at the end)", shown(path)));
        }
    }
    if !problems.is_empty() {
        return Err(format!(
            "i18n: fork catalogs to fix (keys in Unicode code-point order; add a key anywhere, then run `cargo xtask i18n \
             --sort`; docs/localization.md):{problems}"
        ));
    }
    println!("i18n: {} fork catalogs sorted", paths.len());
    Ok(())
}

pub fn sort(root: &Path) -> Result<(), String> {
    let paths = catalogs(root)?;
    let mut sorted = Vec::with_capacity(paths.len());
    let mut problems = String::new();
    for path in &paths {
        match read(path) {
            Ok((text, mut entries)) => {
                entries.sort();
                let new = render(&entries)?;
                if new != text {
                    sorted.push((path, new));
                }
            }
            Err(e) => problems.push_str(&format!("\n  {e}")),
        }
    }
    if !problems.is_empty() {
        return Err(format!("i18n: fork catalogs to fix first; nothing changed:{problems}"));
    }
    for (path, text) in &sorted {
        // through a file beside it and a rename, so a full disk leaves the catalog as it was
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, text).and_then(|()| std::fs::rename(&partial, path)).map_err(|e| {
            let _ = std::fs::remove_file(&partial);
            format!("{}: {e}", shown(path))
        })?;
        println!("i18n: sorted {}", shown(path));
    }
    println!("i18n: {} of {} fork catalogs rewritten, all sorted", sorted.len(), paths.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Entries {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A throwaway repo root holding `locales/fork/<name>` and upstream's `locales/<name>` files.
    struct Root(PathBuf);

    impl Root {
        fn new(test: &str, fork: &[(&str, &str)], main: &[(&str, &str)]) -> Root {
            let root = std::env::temp_dir().join(format!("xtask-i18n-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join(FORK_DIR)).unwrap();
            for (name, text) in fork {
                std::fs::write(root.join(FORK_DIR).join(name), text).unwrap();
            }
            for (name, text) in main {
                std::fs::write(root.join("crates/ui-egui/locales").join(name), text).unwrap();
            }
            Root(root)
        }

        fn fork(&self, name: &str) -> String {
            std::fs::read_to_string(self.0.join(FORK_DIR).join(name)).unwrap()
        }

        fn main(&self, name: &str) -> String {
            std::fs::read_to_string(self.0.join("crates/ui-egui/locales").join(name)).unwrap()
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const UNSORTED: &str = "{\n  \"b\": \"2\",\n  \"a\": \"1\",\n  \"c\": \"3\"\n}\n";
    const SORTED: &str = "{\n  \"a\": \"1\",\n  \"b\": \"2\",\n  \"c\": \"3\"\n}\n";

    #[test]
    fn keys_sort_in_code_point_order() {
        // upper case before lower, ASCII before accented letters and CJK, by code point not by locale
        let shuffled = "{\"é\": \"\", \"b\": \"\", \"Z\": \"\", \"中\": \"\", \"a|x\": \"\", \"a\": \"\", \"{app}\": \"\"}";
        let root = Root::new("code-points", &[("ja.json", shuffled)], &[]);
        sort(&root.0).unwrap();
        let keys: Vec<String> = parse(&root.fork("ja.json")).unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["Z", "a", "a|x", "b", "{app}", "é", "中"]);
    }

    #[test]
    fn rendering_keeps_the_catalog_format() {
        let e = entries(&[("Quote \"x\"", "Zitat „x“"), ("Tab\tand\nline", "日本語 \\ back"), ("Ctrl\u{1}", "ü")]);
        let text = render(&e).unwrap();
        assert_eq!(text, "{\n  \"Quote \\\"x\\\"\": \"Zitat „x“\",\n  \"Tab\\tand\\nline\": \"日本語 \\\\ back\",\n  \"Ctrl\\u0001\": \"ü\"\n}\n");
        assert_eq!(parse(&text).unwrap(), e, "a rendered catalog should read back to the same entries");
        assert_eq!(render(&[]).unwrap(), "{}\n");
    }

    #[test]
    fn a_repeated_key_is_an_error_naming_it() {
        let err = parse("{\n  \"Open\": \"Öffnen\",\n  \"Close\": \"x\",\n  \"Open\": \"Auf\"\n}\n").unwrap_err();
        assert!(err.contains("\"Open\" appears twice"), "the error should name the repeated key: {err}");
        assert!(err.contains("line 4"), "the error should say where: {err}");
    }

    #[test]
    fn broken_catalogs_are_errors_not_panics() {
        for text in ["", "{", "[]", "\"x\"", "{\"a\": 1}", "{\"a\": {\"b\": \"c\"}}", "{\"a\": \"b\",}", "{\"a\": \"b\"} x", "\u{feff}{}"] {
            assert!(parse(text).is_err(), "{text:?} should be refused");
        }
        let err = parse("{\"Rating\": 5}").unwrap_err();
        assert!(err.contains("\"Rating\""), "the error should name the key: {err}");
    }

    #[test]
    fn check_reports_order_format_and_repeated_keys() {
        // check names the file and the first key out of place
        {
            let root = Root::new("check-order", &[("de.json", SORTED), ("fr.json", UNSORTED)], &[]);
            let err = check(&root.0).unwrap_err();
            assert!(err.contains("locales/fork/fr.json: \"a\" is out of order"), "the error should name the file and key: {err}");
            assert!(!err.contains("de.json"), "a sorted catalog should not be reported: {err}");
        }
        // check passes sorted catalogs and refuses another format
        {
            let root = Root::new("check-format", &[("de.json", SORTED), ("de-formats.json", "{}\n")], &[]);
            assert_eq!(check(&root.0), Ok(()));
            let crlf = Root::new("check-crlf", &[("de.json", &SORTED.replace('\n', "\r\n"))], &[]);
            assert_eq!(check(&crlf.0), Ok(()), "a Windows checkout's line endings should pass");
            for (test, text) in
                [("indent", "{\n    \"a\": \"1\"\n}\n"), ("newline", "{\n  \"a\": \"1\"\n}"), ("escape", "{\n  \"a\": \"\\u00fc\"\n}\n")]
            {
                let root = Root::new(test, &[("de.json", text)], &[]);
                let err = check(&root.0).unwrap_err();
                assert!(err.contains("de.json: in order, but not in the catalog format"), "{test}: {err}");
            }
        }
        // check and sort report a repeated key and change nothing
        {
            let dup = "{\n  \"b\": \"2\",\n  \"b\": \"3\"\n}\n";
            let root = Root::new("dup", &[("de.json", dup), ("fr.json", UNSORTED)], &[]);
            let err = check(&root.0).unwrap_err();
            assert!(err.contains("locales/fork/de.json: the key \"b\" appears twice"), "{err}");
            let err = sort(&root.0).unwrap_err();
            assert!(err.contains("locales/fork/de.json: the key \"b\" appears twice"), "{err}");
            assert_eq!(root.fork("de.json"), dup, "a broken catalog should be left as it is");
            assert_eq!(root.fork("fr.json"), UNSORTED, "no catalog should be written while one is broken");
        }
    }

    #[test]
    fn sort_keeps_every_entry_and_a_second_run_changes_nothing() {
        let messy = "{\"zebra\":\"Zebra\",\"Apple\":\"Apfel\",\n\"émigré\":\"x\", \"apple\": \"\"}";
        let root = Root::new("sort", &[("de.json", messy), ("de-formats.json", UNSORTED), ("ja.json", SORTED)], &[]);
        sort(&root.0).unwrap();
        let mut before = parse(messy).unwrap();
        before.sort();
        assert_eq!(parse(&root.fork("de.json")).unwrap(), before, "sorting should keep every key and value");
        assert_eq!(root.fork("de.json"), "{\n  \"Apple\": \"Apfel\",\n  \"apple\": \"\",\n  \"zebra\": \"Zebra\",\n  \"émigré\": \"x\"\n}\n");
        assert_eq!(root.fork("de-formats.json"), SORTED);
        assert_eq!(check(&root.0), Ok(()));
        let once: Vec<String> = ["de.json", "de-formats.json", "ja.json"].iter().map(|n| root.fork(n)).collect();
        sort(&root.0).unwrap();
        let twice: Vec<String> = ["de.json", "de-formats.json", "ja.json"].iter().map(|n| root.fork(n)).collect();
        assert_eq!(once, twice, "a second sort should change nothing");
    }

    #[test]
    fn upstream_catalogs_are_neither_checked_nor_sorted() {
        let root = Root::new("main", &[("de.json", UNSORTED)], &[("de.json", UNSORTED), ("de-formats.json", "{\"b\":\"\",\"a\":\"\"}")]);
        let err = check(&root.0).unwrap_err();
        assert!(!err.contains("locales/de"), "upstream's catalogs should not be checked: {err}");
        sort(&root.0).unwrap();
        assert_eq!(root.fork("de.json"), SORTED);
        assert_eq!(root.main("de.json"), UNSORTED, "upstream's catalog should be left as it is");
        assert_eq!(root.main("de-formats.json"), "{\"b\":\"\",\"a\":\"\"}");
        assert_eq!(check(&root.0), Ok(()), "upstream's unsorted catalogs should not fail the check");
    }
}
