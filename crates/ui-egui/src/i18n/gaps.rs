//! The plain `tr("…")` messages of the app's source, for `i18n::tests::translation_gaps_report`.
//!
//! A catalog only holds the strings someone translated, so a string no language has yet is in no
//! catalog and the report could not name it. This reads the source for them: every `tr("…")` /
//! `.tr("…")` call in `crates/ui-egui/src` and `apps/lightcraft/src` whose argument is one string
//! literal (a trailing comma, as rustfmt leaves in a wrapped call, is fine), or an `if`/`match`
//! whose branches are literals (`tr(if on { "On" } else { "Off" })`). Test code (`tests_*.rs`
//! files, `tests/` folders and items under exactly `#[cfg(test)]`) is skipped. The scanner lexes
//! the source (comments, strings, raw strings and chars) rather than grepping it, so a `tr(` in a
//! comment or a string is not a use.
//!
//! A key that reaches `tr` through a variable (`tr(label)` over a table of labels,
//! `tr(&format!(…))`) cannot be read from the source; the report takes those labels from its own
//! tables. `tr_ctx` keys (`"<context>|<source>"`) are optional by design.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

/// Source folders whose `tr` calls are read, relative to the crate.
const SOURCES: [&str; 2] = ["src", "../../apps/lightcraft/src"];

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Ident(String),
    Str(String),
    Punct(char),
    Other,
}

#[derive(Debug, Clone)]
struct Tok {
    kind: Kind,
    line: usize,
}

/// One `tr` call with a literal key.
#[derive(Debug, Clone, PartialEq)]
struct Use {
    key: String,
    line: usize,
}

/// Split Rust source into the tokens the scan needs. Comments vanish; strings, raw strings and
/// chars are single tokens, so braces and parentheses inside them never count.
fn lex(source: &str) -> Vec<Tok> {
    let chars: Vec<char> = source.replace("\r\n", "\n").chars().collect();
    let mut tokens = Vec::new();
    let (mut i, mut line) = (0, 1);
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if c == '"' {
            let start = line;
            let (text, end, lines) = read_string(&chars, i + 1);
            tokens.push(Tok { kind: Kind::Str(text), line: start });
            line += lines;
            i = end;
        } else if c == '\'' {
            // A char literal (`'x'`, `'\n'`, `'\u{1F600}'`) is one token; anything else is a lifetime.
            let is_char = next == Some('\\') || chars.get(i + 2) == Some(&'\'');
            if is_char {
                i += if next == Some('\\') { 3 } else { 1 };
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
                tokens.push(Tok { kind: Kind::Other, line });
            } else {
                tokens.push(Tok { kind: Kind::Punct('\''), line });
                i += 1;
            }
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let raw_prefix = word == "r" || word == "br";
            let hashes = chars[i..].iter().take_while(|c| **c == '#').count();
            if raw_prefix && chars.get(i + hashes) == Some(&'"') {
                let begin = i + hashes + 1;
                let closing: Vec<char> = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                let end = (begin..chars.len()).find(|at| chars[*at..].starts_with(&closing)).unwrap_or(chars.len());
                let text: String = chars[begin..end].iter().collect();
                let lines = text.matches('\n').count();
                tokens.push(Tok { kind: Kind::Str(text), line });
                line += lines;
                i = (end + closing.len()).min(chars.len());
            } else {
                tokens.push(Tok { kind: Kind::Ident(word), line });
            }
        } else if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            tokens.push(Tok { kind: Kind::Other, line });
        } else {
            tokens.push(Tok { kind: Kind::Punct(c), line });
            i += 1;
        }
    }
    tokens
}

/// Read a (non-raw) string body starting after its opening quote: the decoded text, the index after
/// the closing quote and how many newlines the literal spans.
fn read_string(chars: &[char], mut i: usize) -> (String, usize, usize) {
    let (mut text, mut lines) = (String::new(), 0);
    while i < chars.len() && chars[i] != '"' {
        if chars[i] != '\\' {
            if chars[i] == '\n' {
                lines += 1;
            }
            text.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1;
        match chars.get(i).copied() {
            Some('n') => text.push('\n'),
            Some('t') => text.push('\t'),
            Some('r') => text.push('\r'),
            Some('0') => text.push('\0'),
            Some('x') => {
                let hex: String = chars[i + 1..(i + 3).min(chars.len())].iter().collect();
                text.extend(u8::from_str_radix(&hex, 16).ok().map(char::from));
                i += 2;
            }
            Some('u') => {
                let close = (i..chars.len()).find(|at| chars[*at] == '}').unwrap_or(chars.len() - 1);
                let hex: String = chars[i + 2..close].iter().filter(|c| **c != '_').collect();
                text.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                i = close;
            }
            // A backslash before a newline joins the lines and drops the next line's indentation.
            Some('\n') => {
                lines += 1;
                while chars.get(i + 1).is_some_and(|c| c.is_whitespace()) {
                    if chars[i + 1] == '\n' {
                        lines += 1;
                    }
                    i += 1;
                }
            }
            Some(other) => text.push(other),
            None => {}
        }
        i += 1;
    }
    (text, i + 1, lines)
}

/// The index of the token that closes the group opened at `open`.
fn matching(tokens: &[Tok], open: usize) -> usize {
    let Kind::Punct(opener) = tokens[open].kind else { return open };
    let closer = match opener {
        '(' => ')',
        '{' => '}',
        _ => ']',
    };
    let mut depth = 0;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            Kind::Punct(c) if c == opener => depth += 1,
            Kind::Punct(c) if c == closer => {
                depth -= 1;
                if depth == 0 {
                    return at;
                }
            }
            _ => {}
        }
    }
    tokens.len() - 1
}

fn is_punct(token: Option<&Tok>, c: char) -> bool {
    token.is_some_and(|token| token.kind == Kind::Punct(c))
}

fn is_ident(token: Option<&Tok>, name: &str) -> bool {
    token.is_some_and(|token| matches!(&token.kind, Kind::Ident(word) if word == name))
}

/// Is `tokens[at..]` the attribute `#[cfg(test)]`?
fn is_cfg_test(tokens: &[Tok], at: usize) -> bool {
    is_punct(tokens.get(at), '#')
        && is_punct(tokens.get(at + 1), '[')
        && is_ident(tokens.get(at + 2), "cfg")
        && is_punct(tokens.get(at + 3), '(')
        && is_ident(tokens.get(at + 4), "test")
        && is_punct(tokens.get(at + 5), ')')
        && is_punct(tokens.get(at + 6), ']')
}

/// Does a comma-terminated entry start at `at`: a struct field (or a field in a struct literal,
/// `name: …`) or an enum variant (`Name,` `Name(…)` `Name { … }` `Name = 3`)?
fn starts_entry(tokens: &[Tok], mut at: usize) -> bool {
    if is_ident(tokens.get(at), "pub") {
        at += 1;
        if is_punct(tokens.get(at), '(') {
            at = matching(tokens, at) + 1;
        }
    }
    const STATEMENTS: [&str; 10] = ["if", "let", "match", "for", "while", "loop", "unsafe", "return", "else", "break"];
    let Some(Kind::Ident(name)) = tokens.get(at).map(|token| &token.kind) else { return false };
    if STATEMENTS.contains(&name.as_str()) {
        return false;
    }
    let field = is_punct(tokens.get(at + 1), ':') && !is_punct(tokens.get(at + 2), ':');
    let variant = matches!(tokens.get(at + 1).map(|token| &token.kind), Some(Kind::Punct(',' | '(' | '{' | '=' | '}')));
    field || variant
}

/// The index just past the item that starts at `at` (after its attributes): a struct field, a
/// field in a struct literal or an enum variant ends at its comma, anything else at its closing
/// brace or its `;`. Groups are jumped over, so a `;` in `[u8; 4]` or a `,` in `f(a, b)` ends nothing.
fn skip_item(tokens: &[Tok], mut at: usize) -> usize {
    while is_punct(tokens.get(at), '#') && is_punct(tokens.get(at + 1), '[') {
        at = matching(tokens, at + 1) + 1;
    }
    let entry = starts_entry(tokens, at);
    while let Some(token) = tokens.get(at) {
        match token.kind {
            Kind::Punct('(' | '[') => at = matching(tokens, at) + 1,
            Kind::Punct('{') if entry => at = matching(tokens, at) + 1,
            Kind::Punct('{') => return matching(tokens, at) + 1,
            Kind::Punct(';') => return at + 1,
            Kind::Punct(',') if entry => return at + 1,
            // The enclosing block ends: the last entry has no comma.
            Kind::Punct('}' | ')' | ']') if entry => return at,
            _ => at += 1,
        }
    }
    at
}

/// The literal keys of every non-test `tr(…)` call in one source file.
fn scan_source(source: &str) -> Vec<Use> {
    let tokens = lex(source);
    let mut found = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        if is_cfg_test(&tokens, at) {
            at = skip_item(&tokens, at + 7);
            continue;
        }
        let is_call = is_ident(tokens.get(at), "tr") && is_punct(tokens.get(at + 1), '(') && !(at > 0 && is_ident(tokens.get(at - 1), "fn"));
        if is_call {
            let close = matching(&tokens, at + 1);
            let mut inner = &tokens[at + 2..close];
            // rustfmt wraps a long call as `tr(\n    "…",\n)`: the trailing comma is not an argument.
            if let [rest @ .., Tok { kind: Kind::Punct(','), .. }] = inner {
                inner = rest;
            }
            let mut keys = Vec::new();
            match inner {
                [Tok { kind: Kind::Str(key), line }] => keys.push((key.clone(), *line)),
                // `tr(if flag { "On" } else { "Off" })` and `tr(match x { A => "a", B => { "b" } })`:
                // each literal that is a whole branch is a key.
                [Tok { kind: Kind::Ident(word), .. }, ..] if word == "if" || word == "match" => {
                    for window in inner.windows(3) {
                        if let [before, Tok { kind: Kind::Str(key), line }, after] = window {
                            let braced = before.kind == Kind::Punct('{') && after.kind == Kind::Punct('}');
                            let arm = before.kind == Kind::Punct('>') && matches!(after.kind, Kind::Punct(',' | '}'));
                            if braced || arm {
                                keys.push((key.clone(), *line));
                            }
                        }
                    }
                }
                _ => {}
            }
            found.extend(keys.into_iter().filter(|(key, _)| !key.is_empty()).map(|(key, line)| Use { key, line }));
        }
        at += 1;
    }
    found
}

/// Every `.rs` file under `dir` that is not a test file, in a stable order.
fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        if path.is_dir() {
            if name != "tests" {
                source_files(&path, out);
            }
        } else if name.ends_with(".rs") && !name.starts_with("tests_") {
            out.push(path);
        }
    }
}

/// The literal keys of the `tr(…)` calls in one source file.
pub(super) fn messages_in(source: &str) -> BTreeSet<String> {
    scan_source(source).into_iter().map(|used| used.key).collect()
}

/// The literal key of every `tr("…")` call in the app's non-test source.
pub(super) fn source_messages() -> BTreeSet<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for source in SOURCES {
        assert!(root.join(source).is_dir(), "{source} (relative to the crate) is not a folder: the report has nothing to scan");
        source_files(&root.join(source), &mut files);
    }
    let mut keys = BTreeSet::new();
    for path in files {
        let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        keys.extend(messages_in(&text));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scanner_reads_literals_and_skips_comments_strings_and_tests() {
        let source = r##"
use crate::i18n::tr;
fn f(ui: &mut Ui, on: bool, label: &str) {
    ui.label(tr("Plain"));
    ui.label(crate::i18n::tr("Qualified"));
    ui.label(format!("{}: x", i18n::tr("In a macro")));
    ui.label(Locale::De.tr("Method"));
    ui.label(tr("Two \"quoted\" \u{2605} words"));
    ui.label(tr("Wrapped \
                 line"));
    ui.label(tr(r#"Raw "string""#));
    ui.label(tr(if on { "On" } else { "Off" }));
    ui.label(tr(label));
    ui.label(tr(&format!("{label}")));
    // tr("In a line comment")
    /* tr("In a /* nested */ block comment") */
    let s = "tr(\"In a string\")"; let c = '"'; let r = &'a str;
    ui.label(tr("After the char and lifetime"));
}
fn tr(source: &str) -> &str { source }
#[cfg(test)]
mod tests {
    fn t() { tr("In a test module"); }
}
#[cfg(test)]
fn only_in_tests() { tr("In a test fn"); }
fn last() { tr("Last"); }
"##;
        let keys: Vec<(String, usize)> = scan_source(source).into_iter().map(|used| (used.key, used.line)).collect();
        let keys: Vec<(&str, usize)> = keys.iter().map(|(key, line)| (key.as_str(), *line)).collect();
        assert_eq!(
            keys,
            vec![
                ("Plain", 4),
                ("Qualified", 5),
                ("In a macro", 6),
                ("Method", 7),
                ("Two \"quoted\" \u{2605} words", 8),
                ("Wrapped line", 9),
                ("Raw \"string\"", 11),
                ("On", 12),
                ("Off", 12),
                ("After the char and lifetime", 18),
                ("Last", 27),
            ]
        );
    }

    #[test]
    fn the_scanner_reads_wrapped_calls_and_match_arms_and_skips_only_the_test_item() {
        let source = r#"
fn f(m: Mode) {
    ui.label(crate::i18n::tr(
        "Wrapped by rustfmt",
    ));
    ui.label(tr(match m { Mode::A => "Arm", Mode::B => { "Braced arm" }, Mode::C => "Last arm" }));
    ui.label(tr(match m { Mode::A => other(), Mode::B => "Mixed arm" }));
    let r = Renderer {
        #[cfg(test)]
        built: 0,
        after: tr("After a test-only field"),
    };
}
struct S {
    #[cfg(test)]
    seen: u32
}
fn g() { tr("After a last test-only field"); }
enum E {
    A,
    #[cfg(test)]
    B { x: u8 },
    #[cfg(test)]
    C(u8),
    #[cfg(test)]
    D = 3,
    F,
}
fn after_enum() { tr("After test-only variants"); }
#[cfg(test)]
fn h() -> [u8; 4] { tr("Test only"); [0; 4] }
#[cfg(not(test))]
fn n() { tr("Not a test"); }
"#;
        let source = format!("{source}fn crlf() {{\r\n    tr(\"Crlf \\\r\n      line\");\r\n}}\r\n");
        let keys: Vec<String> = scan_source(&source).into_iter().map(|used| used.key).collect();
        assert_eq!(
            keys,
            [
                "Wrapped by rustfmt",
                "Arm",
                "Braced arm",
                "Last arm",
                "Mixed arm",
                "After a test-only field",
                "After a last test-only field",
                "After test-only variants",
                "Not a test",
                "Crlf line"
            ]
        );
    }
}
