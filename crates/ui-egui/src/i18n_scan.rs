//! Test-only reader of the UI's source: the English messages it hands to `tr`, directly or through a
//! helper that translates its text argument, in every non-test file under `src/`. The coverage tests
//! (`tests_i18n_coverage`) and the translation gaps report (`i18n`) read the UI's messages from here.
//!
//! A helper is found by its body: a function whose `&str` parameter goes to `tr(` translates that
//! argument. Private helpers count in their own file, public ones wherever they are called by path
//! (`widgets::section_header(…)`) or as a method. Only string literals are read; text built at run
//! time, or passed in a variable, is the painted-screens test's (`tests_i18n_sweep`) to find.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A UI source file: its path under `src/` and its code without its test module.
pub struct Source {
    pub file: String,
    pub code: String,
}

/// Every non-test `.rs` file under `src/` (test files and `tests/` folders skipped, a trailing
/// `#[cfg(test)] mod tests` cut off).
pub fn sources() -> Vec<Source> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        for path in std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path()) {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            if path.is_dir() {
                if name != "tests" {
                    dirs.push(path);
                }
            } else if name.ends_with(".rs") && !name.starts_with("tests_") && name != "tests.rs" && name != "i18n_scan.rs" {
                files.push(path);
            }
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|path| {
            // read as LF: a Windows checkout (`core.autocrlf`) has CRLF line ends, and the test
            // module must be cut off there too
            let text = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
            let code = text.split("#[cfg(test)]\nmod tests {").next().unwrap_or_default().to_string();
            let file = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
            Source { file, code }
        })
        .collect()
}

/// The Rust string literal starting at `s` (just after its opening quote), unescaped, and the bytes
/// it took (closing quote included); `None` for anything this little reader doesn't understand.
pub fn literal(s: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, i + 1)),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '\'' => out.push('\''),
                'u' => {
                    // \u{XXXX}
                    let rest = &s[i + 2..];
                    let end = rest.find('}')?;
                    let code = u32::from_str_radix(rest.get(1..end)?, 16).ok()?;
                    out.push(char::from_u32(code)?);
                    for _ in 0..=end {
                        chars.next();
                    }
                }
                '\n' => {
                    // a line continuation: the next line's leading spaces are dropped
                    while chars.clone().next().is_some_and(|(_, c)| c.is_whitespace()) {
                        chars.next();
                    }
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// The arguments of the call whose `(` was just before `s`, split at top-level commas (strings,
/// chars and brackets understood well enough for this code base).
fn args(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let (mut depth, mut start, mut i) = (0usize, 0usize, 0usize);
    let mut out = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b'"' => match literal(&s[i + 1..]) {
                Some((_, len)) => i += len,
                None => {
                    // an escape the reader doesn't know: skip to the closing quote
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j] != b'"' {
                        j += if bytes[j] == b'\\' { 2 } else { 1 };
                    }
                    i = j;
                }
            },
            // a char literal ('x', '\n', '"'), not a lifetime ('a)
            b'\'' if bytes.get(i + 2) == Some(&b'\'') => i += 2,
            b'\'' if bytes.get(i + 1) == Some(&b'\\') => {
                while i + 1 < bytes.len() && bytes[i + 1] != b'\'' {
                    i += 1;
                }
                i += 1;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => {
                out.push(&s[start..i]);
                return out;
            }
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The text of an argument that is a string literal (`"…"`, `Some("…")`, `&"…"`).
fn literal_arg(arg: &str) -> Option<String> {
    let arg = arg.trim();
    let arg = arg.strip_prefix("Some(").and_then(|a| a.strip_suffix(')')).unwrap_or(arg).trim();
    let arg = arg.strip_prefix('&').unwrap_or(arg);
    let (text, len) = literal(arg.strip_prefix('"')?)?;
    (len + 1 == arg.len()).then_some(text)
}

/// The texts an argument can be: a literal, or each branch of `if … { "a" } else { "b" }` whose
/// block is a literal alone.
fn literal_args(arg: &str) -> Vec<String> {
    if let Some(text) = literal_arg(arg) {
        return vec![text];
    }
    if !arg.trim_start().starts_with("if ") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (at, _) in arg.match_indices('{') {
        let rest = &arg[at + 1..];
        if let Some(end) = rest.find('}')
            && let Some(text) = literal_arg(&rest[..end])
        {
            out.push(text);
        }
    }
    out
}

/// A function that translates one of its parameters.
#[derive(Clone, Debug)]
struct Helper {
    name: String,
    file: String,
    public: bool,
    /// Its position among the call's arguments (a method's `self` not counted).
    arg: usize,
    method: bool,
}

/// Functions whose `&str` (or `Option<&str>`, `impl Into<String>`) parameter is passed to `tr(`,
/// and a file's macros that pass one of their fragments to it ([`macro_helpers`]).
fn helpers(sources: &[Source]) -> Vec<Helper> {
    let mut out = macro_helpers(sources);
    for source in sources {
        let code = &source.code;
        let mut rest = 0;
        while let Some(at) = code[rest..].find("fn ") {
            let at = rest + at;
            rest = at + 3;
            // `fn` as a word, at the start of an item
            if code[..at].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let after = &code[at + 3..];
            let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let Some(open) = after.find('(') else { continue };
            if name.is_empty() || after[name.len()..open].contains(['{', ';']) {
                continue;
            }
            let params = args(&after[open + 1..]);
            let public = code[..at].rsplit('\n').next().is_some_and(|line| line.trim_start().starts_with("pub"));
            let method = params.first().is_some_and(|p| p.contains("self"));
            // the body: up to the next item at the same indent, at most a few thousand bytes
            let body_start = at + 3 + open;
            let body = code.get(body_start..(body_start + 4000).min(code.len())).unwrap_or_default();
            let body = body
                .get(1..)
                .and_then(|b| b.find("\nfn ").or_else(|| b.find("\n    fn ")).or_else(|| b.find("\npub")).map(|end| &b[..end]))
                .unwrap_or(body);
            for (i, param) in params.iter().enumerate() {
                let Some((pname, ty)) = param.split_once(':') else { continue };
                let (pname, ty) = (pname.trim().trim_start_matches("mut ").trim(), ty.trim());
                if !(ty.ends_with("str") || ty.starts_with("Option<&") || ty.starts_with("impl Into<String>")) {
                    continue;
                }
                let calls_tr = body.match_indices("tr(").any(|(j, _)| {
                    let before_ok = !body[..j].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_');
                    // `tr(text)`, `tr(&text)`
                    let arg = body[j + 3..].trim_start().trim_start_matches('&');
                    before_ok && arg.starts_with(pname) && !arg[pname.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                });
                if calls_tr {
                    let arg = if method { i.saturating_sub(1) } else { i };
                    out.push(Helper { name: name.clone(), file: source.file.clone(), public, arg, method });
                }
            }
        }
    }
    out
}

/// A file's own `macro_rules!` whose first arm passes one of its fragments to `tr(` (`trf!("Immich:
/// {}", e)`, which formats the translated literal): named with its `!`, called in that file only, the
/// argument at the fragment's place among the arm's top-level fragments.
fn macro_helpers(sources: &[Source]) -> Vec<Helper> {
    let mut out = Vec::new();
    for source in sources {
        let code = &source.code;
        for (at, _) in code.match_indices("macro_rules!") {
            let after = code[at + "macro_rules!".len()..].trim_start();
            let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            // the first arm: its matcher `( … )`, then `=> { … }`
            let Some(open) = after.find('(') else { continue };
            let matcher = args(&after[open + 1..]).join(",");
            let rest = &after[open + 1 + matcher.len()..];
            let Some(arrow) = rest.find("=>") else { continue };
            let Some(body_open) = rest[arrow..].find(['{', '(', '[']) else { continue };
            let body = args(&rest[arrow + body_open + 1..]).join(",");
            // the matcher's top-level fragments (`$t:literal`), not those inside a repetition `$( … )*`
            let mut fragments = Vec::new();
            let mut depth = 0usize;
            for (i, c) in matcher.char_indices() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth = depth.saturating_sub(1),
                    '$' if depth == 0 => {
                        let fragment: String = matcher[i + 1..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                        if !fragment.is_empty() {
                            fragments.push(fragment);
                        }
                    }
                    _ => {}
                }
            }
            for (arg, fragment) in fragments.iter().enumerate() {
                let calls_tr = body.match_indices("tr(").any(|(j, _)| {
                    let before_ok = !body[..j].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_');
                    let passed = body[j + 3..].trim_start().trim_start_matches('&');
                    before_ok
                        && passed
                            .strip_prefix('$')
                            .and_then(|p| p.strip_prefix(fragment.as_str()))
                            .is_some_and(|tail| !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
                });
                if !name.is_empty() && calls_tr {
                    out.push(Helper { name: format!("{name}!"), file: source.file.clone(), public: false, arg, method: false });
                }
            }
        }
    }
    out
}

/// The module path of a source file (`panels/settings.rs` → `panels::settings`, `panels/mod.rs` →
/// `panels`, `lib.rs` → the crate root).
fn module_of(file: &str) -> Vec<String> {
    let mut parts: Vec<String> = file.trim_end_matches(".rs").split('/').map(str::to_string).collect();
    if matches!(parts.last().map(String::as_str), Some("mod" | "lib")) {
        parts.pop();
    }
    parts
}

/// The helpers a call of `name` at byte `at` of `source` may be: a method call (`.name(`) any
/// method of that name; a path call (`a::b::name(`) the helper in that module; a bare call one in
/// the same file, or one the file imports by name.
fn resolve<'h>(helpers: &'h [Helper], source: &Source, name: &str, at: usize) -> Vec<&'h Helper> {
    let before = &source.code[..at];
    let named = helpers.iter().filter(|h| h.name == name && (h.public || h.file == source.file));
    if before.ends_with('.') {
        return named.filter(|h| h.method).collect();
    }
    if before.ends_with("::") {
        // the path's segments, nearest first
        let path: Vec<&str> = before
            .trim_end_matches("::")
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
            .next()
            .unwrap_or_default()
            .split("::")
            .collect();
        let mut module = module_of(&source.file);
        let mut segments = path.iter();
        let target: Vec<String> = match path.first().copied() {
            Some("crate") => {
                segments.next();
                segments.map(|s| s.to_string()).collect()
            }
            Some("super") | Some("self") => {
                for s in segments.by_ref() {
                    match *s {
                        "super" => {
                            module.pop();
                        }
                        "self" => {}
                        other => module.push(other.to_string()),
                    }
                }
                module
            }
            // an imported module (`widgets::row`): matched by its last segment
            _ => return named.filter(|h| !h.method && module_of(&h.file).last().map(String::as_str) == path.last().copied()).collect(),
        };
        return named.filter(|h| !h.method && module_of(&h.file) == target).collect();
    }
    named
        .filter(|h| {
            !h.method
                && (h.file == source.file
                    || source.code.lines().any(|line| {
                        line.trim_start().starts_with("use ")
                            && line.contains(name)
                            && module_of(&h.file).last().is_some_and(|m| line.contains(m.as_str()))
                    }))
        })
        .collect()
}

/// Every literal the UI hands to a translating call, with the files it was seen in: `tr("…")`,
/// `tr_format!("…")`, `crate::button::Button`'s text and tooltip, and the helpers found above.
pub fn translated_texts() -> BTreeMap<String, BTreeSet<String>> {
    let sources = sources();
    let helpers = helpers(&sources);
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut add = |arg: &str, file: &str| {
        for text in literal_args(arg) {
            // a sample value (`logo.png`, a URL, `a/b`) passes through `tr` unchanged; it isn't a message
            let sample = !text.contains(char::is_whitespace) && text.contains(['.', '/', ':']);
            if text.chars().any(char::is_alphabetic) && !sample {
                found.entry(text).or_default().insert(file.to_string());
            }
        }
    };
    for source in &sources {
        let code = &source.code;
        for (at, _) in code.match_indices('(') {
            let before = &code[..at];
            let name: String = {
                let rev: String = before.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '!').collect();
                rev.chars().rev().collect()
            };
            if name.is_empty() {
                continue;
            }
            let head = &before[..before.len() - name.len()];
            if head.ends_with("fn ") {
                continue;
            }
            let call_args = || args(&code[at + 1..]);
            match name.as_str() {
                "tr" | "tr_format!" => {
                    if let Some(arg) = call_args().first() {
                        add(arg, &source.file);
                    }
                }
                // `crate::button::Button` translates its text and tooltip; egui's doesn't
                "new" if head.ends_with("Button::") && !head.ends_with("egui::Button::") => {
                    if let Some(arg) = call_args().first() {
                        add(arg, &source.file);
                    }
                }
                "tip" if head.ends_with('.') => {
                    if let Some(arg) = call_args().first() {
                        add(arg, &source.file);
                    }
                }
                _ => {
                    let helpers = resolve(&helpers, source, &name, at - name.len());
                    if !helpers.is_empty() {
                        let call_args = call_args();
                        for h in helpers {
                            if let Some(arg) = call_args.get(h.arg) {
                                add(arg, &source.file);
                            }
                        }
                    }
                }
            }
        }
    }
    found
}

#[test]
fn the_scan_reads_calls_helpers_and_buttons() {
    assert_eq!(args(r#""a, b", (1, 2), ',' ) tail"#), vec![r#""a, b""#, " (1, 2)", " ',' "]);
    assert_eq!(literal_arg(r#" Some("Zoom \u{2192} In") "#).as_deref(), Some("Zoom → In"));
    assert_eq!(literal_arg(r#""a" + b"#), None, "only a whole literal is a message");
    assert_eq!(literal_args(r#" if z { "Zoom In" } else { "Zoom Out" }"#), vec!["Zoom In", "Zoom Out"]);
    assert!(literal_args(r#"&f("{n} file{}", if n == 1 { "" } else { "s" })"#).is_empty(), "a branch inside another call isn't the argument");
    assert_eq!(module_of("panels/mod.rs"), vec!["panels"]);
    assert_eq!(module_of("panels/settings.rs"), vec!["panels", "settings"]);
    let texts = translated_texts();
    // `crate::button::Button` translates its own text (`button.rs`); `widgets::section_header` is a
    // public helper; `tr` itself, everywhere
    for (text, why) in [("Install from File…", "crate Button"), ("Map Style", "access helper"), ("No horizon or vertical lines to level by", "tr")]
    {
        assert!(texts.contains_key(text), "{why}: {text:?} not found");
    }
    // a file's own macro that translates its literal (`trf!` in the Connections panel)
    let (text, file) = ("Immich: {}", "panels/connections.rs");
    assert!(texts.get(text).is_some_and(|files| files.contains(file)), "trf! in {file}: {text:?} not found");
    // `super::row(…, "previousExport", …)` in a panel is the panels' own row, whose id isn't text
    assert!(!texts.contains_key("previousExport"), "a path call resolves to its own module's helper");
}
