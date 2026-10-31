//! `cargo xtask type`: the interface sets its text in the sizes of `theme::TextSize`.
//!
//! Outside `crates/ui-egui/src/theme.rs`, a line of the interface's code may not give a font a
//! size in numbers past the theme (`.size(11.5)`, `FontId::proportional(13.0)`,
//! `FontId::new(13.0, …)`, `FontId::monospace(12.0)`): that is how text comes to be off the scale.
//! Name the size (`t.caption()`, `t.body()`).
//!
//! A size asked of the theme that is not on the scale (`t.font(12.5)`, `t.semibold(13.5)`) is
//! drawn at the nearest one, so it does no harm on screen: such lines are counted and listed with
//! `--list`, not refused, though the number is then not what is drawn.
//!
//! The scale is read from `TextSize::points` in theme.rs. Test code is left alone.

use std::path::Path;

/// The sizes of the scale, read from the arms of `TextSize::points` (`TextSize::Body => 13.0,`).
pub fn scale(theme: &str) -> Vec<f32> {
    theme
        .lines()
        .filter_map(|line| {
            let (name, points) = line.trim().strip_prefix("TextSize::")?.split_once(" => ")?;
            let named = !name.is_empty() && name.chars().all(char::is_alphanumeric);
            points.strip_suffix(',').filter(|_| named)?.parse::<f32>().ok()
        })
        .collect()
}

/// Where a raw string that starts at `at` (`r"`, `r#"`, `br"`, `br##"` …) opens: the index of its
/// opening quote and its number of `#`. Not when the `r` or `b` ends a longer word (`bar"x"`).
fn raw_string(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    if at.checked_sub(1).and_then(|before| bytes.get(before)).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
        return None;
    }
    let mut i = at;
    if bytes.get(i) == Some(&b'b') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hashes = bytes.get(i..)?.iter().take_while(|c| **c == b'#').count();
    let open = i + hashes;
    (bytes.get(open) == Some(&b'"')).then_some((open, hashes))
}

/// The index of the quote that closes a raw string whose contents start at `from`, opened with
/// `hashes` `#`; the end of the text when it is never closed.
fn raw_end(bytes: &[u8], from: usize, hashes: usize) -> usize {
    let mut at = from;
    while let Some(&c) = bytes.get(at) {
        if c == b'"' && bytes.get(at + 1..at + 1 + hashes).is_some_and(|after| after.iter().all(|h| *h == b'#')) {
            return at;
        }
        at += 1;
    }
    bytes.len()
}

/// `source` with everything that is not code to be checked blanked out, byte for byte (so that
/// offsets and lines stay): comments, what strings and characters say, and each item under a
/// `#[cfg(test)]` (a `mod tests;` line, or a block to its closing brace).
fn code(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for b in out.iter_mut().take(to).skip(from) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = bytes.iter().skip(i).position(|c| *c == b'\n').map_or(bytes.len(), |n| i + n);
                blank(&mut out, i, end);
                i = end;
            }
            // a raw string (`r"C:\"`, `r#"…"#`, `br"…"`): no escapes, and it ends at a quote followed
            // by as many `#` as it was opened with
            b'r' | b'b' if raw_string(bytes, i).is_some() => {
                let (open, hashes) = raw_string(bytes, i).unwrap_or((i, 0));
                let close = raw_end(bytes, open + 1, hashes);
                blank(&mut out, open + 1, close);
                i = (close + 1 + hashes).min(bytes.len());
            }
            b'"' => {
                let mut end = i + 1;
                while let Some(&c) = bytes.get(end) {
                    match c {
                        b'\\' => end += 2,
                        b'"' => break,
                        _ => end += 1,
                    }
                }
                blank(&mut out, i + 1, end.min(bytes.len()));
                i = end + 1;
            }
            // a character ('"', '\'', '/'): not a lifetime, which has no closing quote two or three on
            b'\'' => {
                let len = if bytes.get(i + 1) == Some(&b'\\') { 3 } else { 2 };
                if bytes.get(i + len) == Some(&b'\'') {
                    blank(&mut out, i + 1, i + len);
                    i += len + 1;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    // (on the blanked text: a brace in a string or a comment is gone by now)
    const TEST: &[u8] = b"#[cfg(test)]";
    let mut from = 0;
    while let Some(at) = out.get(from..).and_then(|rest| rest.windows(TEST.len()).position(|w| w == TEST)).map(|n| from + n) {
        let after = at + TEST.len();
        let mut paren = 0usize;
        let Some(open) = out
            .iter()
            .skip(after)
            .position(|c| {
                match c {
                    b'(' | b'[' => paren += 1,
                    b')' | b']' => paren = paren.saturating_sub(1),
                    b';' | b'{' | b',' if paren == 0 => return true,
                    _ => {}
                }
                false
            })
            .map(|n| after + n)
        else {
            break;
        };
        let mut end = open + 1;
        if out.get(open) == Some(&b'{') {
            let mut depth = 1usize;
            while let Some(&c) = out.get(end) {
                end += 1;
                match c {
                    b'{' => depth += 1,
                    b'}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
            }
        }
        blank(&mut out, at, end.min(bytes.len()));
        from = end;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The number that is the argument starting at `open` (an index just past a `(` or a `:`), on
/// that line or the next: `13.0`, `11.5_f32`, `12`.
fn number_at(code: &str, open: usize) -> Option<f32> {
    let rest = code.get(open..)?.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    let digits = rest.get(..end)?;
    if !digits.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    digits.trim_end_matches('.').parse().ok()
}

/// What one file of the interface does with text sizes, a line each (`path:line: what, and what
/// to write`): `refused` are sizes given past the theme, `nearest` sizes asked of the theme that
/// it draws at another.
#[derive(Debug, Default, PartialEq)]
pub struct Found {
    pub refused: Vec<String>,
    pub nearest: Vec<String>,
}

/// Checks the numbers written where a font is made. A size held in a constant or a variable is
/// not seen: the tests of what is drawn (`tests/type_scale.rs`) are what finds those.
pub fn check(path: &str, source: &str, scale: &[f32]) -> Found {
    const PAST_THE_THEME: [&str; 4] = [".size(", "FontId::proportional(", "FontId::monospace(", "FontId::new("];
    const THROUGH_THE_THEME: [&str; 2] = [".font(", ".semibold("];
    let code = code(source);
    let line = |at: usize| code.get(..at).map_or(1, |before| before.matches('\n').count() + 1);
    let mut refused = Vec::new();
    let mut refuse = |at: usize, points: f32| {
        refused.push((at, format!("{path}:{}: a text size of its own ({points}): name a size of the scale (t.caption(), t.body(), …)", line(at))));
    };
    for needle in PAST_THE_THEME {
        for (i, _) in code.match_indices(needle) {
            // (a spinner has a size too, and it is not text)
            let spinner = needle == ".size(" && code.get(i.saturating_sub(60)..i).is_some_and(|before| before.contains("Spinner"));
            if let Some(points) = number_at(&code, i + needle.len()).filter(|_| !spinner) {
                refuse(i, points);
            }
        }
    }
    for (i, _) in code.match_indices("FontId {") {
        let field = code.get(i + "FontId {".len()..).map(str::trim_start).and_then(|rest| rest.strip_prefix("size:"));
        if let Some(points) = field.and_then(|rest| number_at(rest, 0)) {
            refuse(i, points);
        }
    }
    let mut nearest = Vec::new();
    for needle in THROUGH_THE_THEME {
        for (i, _) in code.match_indices(needle) {
            if let Some(points) = number_at(&code, i + needle.len()).filter(|p| !scale.contains(p)) {
                nearest.push((i, format!("{path}:{}: {points} pt is drawn at the nearest size of the scale: name the size it is to be", line(i))));
            }
        }
    }
    let in_order = |mut lines: Vec<(usize, String)>| {
        lines.sort();
        lines.into_iter().map(|(_, line)| line).collect()
    };
    Found { refused: in_order(refused), nearest: in_order(nearest) }
}

/// The source files of the interface, test modules (`tests/`, `tests.rs`, `tests_*.rs`, `*_tests.rs`) left out.
fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if path.is_dir() {
            if name != "tests" {
                sources(&path, out)?;
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.starts_with("tests_") && !name.ends_with("_tests.rs") && name != "theme.rs" {
            out.push(path);
        }
    }
    Ok(())
}

pub fn run(root: &Path, list: bool) -> Result<(), String> {
    let ui = root.join("crates/ui-egui/src");
    let theme = ui.join("theme.rs");
    let scale = scale(&std::fs::read_to_string(&theme).map_err(|e| format!("{}: {e}", theme.display()))?);
    if scale.is_empty() {
        return Err(format!("{}: no `TextSize::… => points,` arms found: where is the scale?", theme.display()));
    }
    let mut files = Vec::new();
    sources(&ui, &mut files)?;
    files.sort();
    let mut found = Found::default();
    for file in &files {
        let source = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let shown = file.strip_prefix(root).unwrap_or(file).display().to_string();
        let Found { refused, nearest } = check(&shown, &source, &scale);
        found.refused.extend(refused);
        found.nearest.extend(nearest);
    }
    if list {
        found.nearest.iter().for_each(|line| println!("{line}"));
    }
    for line in &found.refused {
        eprintln!("{line}");
    }
    if !found.refused.is_empty() {
        return Err(format!("{} text size(s) given past the theme (see crates/ui-egui/src/theme.rs, TextSize)", found.refused.len()));
    }
    println!("type: {} files give no text a size past the scale {scale:?}", files.len());
    if !found.nearest.is_empty() && !list {
        println!("type: {} size(s) asked by number are drawn at the nearest size (cargo xtask type --list)", found.nearest.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: [f32; 7] = [10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 18.0];

    #[test]
    fn the_scale_is_read_from_the_theme() {
        let theme = "match self {\n    TextSize::Micro => 10.0,\n    TextSize::Body => 13.0,\n}\nlet x = TextSize::nearest(2.0);\nTextSize::Body => t.body(),";
        assert_eq!(scale(theme), vec![10.0, 13.0]);
        let real = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/ui-egui/src/theme.rs")).unwrap();
        assert_eq!(scale(&real), SCALE);
    }

    #[test]
    fn a_size_given_past_the_theme_is_refused() {
        // Given code that gives text a size in numbers
        let source = "ui.label(RichText::new(a).size(11.5));\nlet f = egui::FontId::proportional(13.0);\nlet g = FontId::new( 12.0, family);\nFontId::monospace(12.0)";
        // When it is checked
        let Found { refused, nearest } = check("panels/x.rs", source, &SCALE);
        // Then each line is refused, on or off the scale
        assert!(nearest.is_empty(), "{nearest:?}");
        assert_eq!(refused.len(), 4, "{refused:?}");
        assert!(refused[0].starts_with("panels/x.rs:1: ") && refused[0].contains("11.5"), "{refused:?}");
        assert!(refused[2].starts_with("panels/x.rs:3: "), "{refused:?}");
    }

    #[test]
    fn a_size_asked_of_the_theme_off_the_scale_is_listed_and_not_refused() {
        // Given code that asks the theme for sizes by number, two of them between sizes
        let source = "p.text(at, a, s, t.font(12.5), c);\nlet f = t.semibold(13.5);\nlet ok = (t.font(13.0), tokens.semibold(11.0));";
        // When it is checked
        let Found { refused, nearest } = check("a.rs", source, &SCALE);
        // Then nothing is refused (the theme draws them on the scale), and the two are listed
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(nearest.len(), 2, "{nearest:?}");
        assert!(nearest[0].contains("a.rs:1") && nearest[0].contains("12.5") && nearest[1].contains("a.rs:2"), "{nearest:?}");
    }

    #[test]
    fn code_after_the_declaration_or_the_block_of_a_test_module_is_checked() {
        // Given a file that declares its test modules first, as the interface's lib.rs does
        let declared = "#[cfg(test)]\nmod tests_a;\n#[cfg(test)]\nmod tests_b;\n\npub fn f() { let x = RichText::new(a).size(11.5); }\n";
        // Then what follows them is checked
        assert_eq!(check("lib.rs", declared, &SCALE).refused.len(), 1);
        // And given a test module in the middle of a file, with a brace in a string in it
        let inline = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let s = \"}\"; let f = egui::FontId::proportional(9.0); }\n}\nfn b() { let f = egui::FontId::proportional(9.5); }\n";
        // Then the module is left alone and the code after it is checked
        let found = check("w.rs", inline, &SCALE).refused;
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("w.rs:6: "), "{found:?}");
    }

    #[test]
    fn a_number_on_the_next_line_and_a_font_written_out_are_found() {
        let source = "let a = RichText::new(x)\n    .size(\n        11.5,\n    );\nlet f = egui::FontId { size: 9.5, family };\n";
        let found = check("a.rs", source, &SCALE).refused;
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("a.rs:2: ") && found[1].starts_with("a.rs:5: "), "{found:?}");
    }

    #[test]
    fn a_spinner_a_string_and_a_comment_after_code_are_not_text_sizes() {
        let source = "ui.add(egui::Spinner::new().size(16.0));\nlet s = \"see .size(3) in docs\";\nlet f = t.caption(); // was .size(11.5)\nlet c = '\"'; let g = t.font(13.0);\n";
        assert_eq!(check("a.rs", source, &SCALE), Found::default());
    }

    #[test]
    fn named_sizes_sizes_of_other_things_comments_and_tests_pass() {
        let source = "\
let a = RichText::new(x).font(t.caption()).color(c);
let b = galley.size().x + rect.size().y;
let c = FontId::new(size.points(), family);
let d = image.size();
let e = t.font(size);
// was .size(11.5) and t.font(12.5)
let f = vec.resize(4.0);
#[cfg(test)]
mod tests {
    fn t() { let f = egui::FontId::proportional(9.0); }
}";
        assert_eq!(check("a.rs", source, &SCALE), Found::default());
    }

    #[test]
    fn a_raw_string_does_not_hide_the_code_after_it() {
        // Given raw strings that end in a backslash, which an escaped string would read past
        // (the closing quote taken as escaped), with a size in numbers after each
        for raw in ["r\"C:\\\"", "r#\"a\\\"#", "br\"x\\\"", "r##\"has \"# inside\\\"##", "br#\"\\\"#"] {
            let source = format!("let p = {raw}; let t = RichText::new(a).size(11.5);\n");
            // When it is checked
            let Found { refused, .. } = check("a.rs", &source, &SCALE);
            // Then the size is still refused
            assert_eq!(refused.len(), 1, "{raw}: {refused:?}");
        }
        // And what a raw string says is not code, a quote inside it ends nothing, and a brace in
        // it does not end a test module early
        let source = "let a = r#\"say \".size(11.5)\" here\"#;\n#[cfg(test)]\nmod tests {\n    fn t() { let s = r\"}\"; let f = FontId::proportional(9.0); }\n}\nfn b() { let f = FontId::proportional(9.5); }\n";
        let Found { refused, .. } = check("a.rs", source, &SCALE);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].starts_with("a.rs:6: "), "{refused:?}");
        // And an identifier that ends in r before a string is not a raw string, and a raw string
        // that is never closed ends the file without a panic
        assert_eq!(check("a.rs", "let s = bar\"x\"; let t = RichText::new(a).size(12.0);", &SCALE).refused.len(), 1);
        for open in ["r\"abc", "r#\"abc\"", "br##\"x\"#", "r#", "r", "br#"] {
            let _ = check("a.rs", open, &SCALE);
        }
    }
}
