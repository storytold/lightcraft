//! Refused commands are said, not dropped (`cargo xtask refusals`).
//!
//! The interface runs commands for the person at it. `LightcraftApp::run` hands the error back;
//! written as `let _ = app.run(…)` a refusal went nowhere: the click did nothing and nobody said
//! why. A widget now says which it means: `app.act(…)` (a toast gives the reason),
//! `app.quiet(…)` (a refusal is expected here; the call says why), or `run` with the `Result`
//! dealt with. This check keeps the dropped form out of the interface's code.
//!
//! Going round `run` drops it just the same: `let _ = app.session.execute(…)` is flagged too.
//! Where that is meant (not a person's action, or a refusal that means nothing to them), a
//! comment on the same line or the line directly above says so, with the reason:
//! `// refusal expected: the task may have finished by itself`.
//!
//! Test code may drop what it likes: `tests.rs`, `tests_*.rs` and `*_tests.rs` files and `tests/` folders aren't read, and whatever a
//! `#[cfg(test)]` applies to (a module, a `mod x;`, a fn, a field, a statement) is skipped, and
//! only that. `apps/lightcraft-web` is left out on purpose: what drops results there is its
//! benchmark (`bench.rs`), which times commands and has nobody to tell.
//!
//! This reads Rust as text, not as syntax. What is approximated:
//! - comments, strings (raw ones too) and char literals are blanked before anything is looked
//!   for; a `'` is taken for a char literal when it closes within one character or starts with a
//!   backslash, and for a lifetime otherwise;
//! - the item after `#[cfg(test)]` ends at the first `;` or `,` outside brackets (a `,` also
//!   outside `<…>`, where `<` and `>` are counted as brackets unless the `>` belongs to `->` or
//!   `=>`) or, for an attribute written inside a `<…>` list (a test-only generic parameter, the last
//!   of its list), at that list's closing `>`, or, when a `{` comes first, at the brace that closes
//!   it; so `a < b` in a test-only constant can stretch the skip as far as the next `;` or `{…}`;
//!   once a `where` is seen outside brackets its commas (between predicates) end nothing: the item
//!   runs to its `{` or `;`;
//! - "inside a `<…>` list" is read by looking back from the attribute to the nearest `<` nothing has
//!   closed, stopping at `{`, `}`, `;` or an unmatched bracket; so a `<` comparison in an earlier
//!   comma-separated sibling (an argument, a match arm) makes the attribute look like it is in a
//!   list, and a `>` comparison in the test-only item ends its skip early: test code is then
//!   checked, never production code hidden;
//! - only the exact attribute `#[cfg(test)]` (or `#![cfg(test)]` for a whole file) counts as
//!   test code, not `cfg(all(test, …))` or a `cfg_attr`;
//! - a dropped result is a `let _ =` whose expression, up to its first `;`, contains one of the
//!   calls below with white space taken out. `_ = …`, `.ok();` and `drop(…)` are not looked for.

use std::path::{Path, PathBuf};

/// Where the interface's code is.
const DIRS: &[&str] = &["crates/ui-egui/src", "apps/lightcraft/src"];

/// A result dropped on the floor: what follows `let _ =` runs a command.
const RUNS: &[&str] = &["app.run(", "self.run(", "app.run_typed(", "self.run_typed(", "run_item("];

/// The same, going round `run` (and so round the interface's own commands and the toast).
const EXECUTES: &[&str] = &[".session.execute("];

/// What excuses a dropped `session.execute`, followed by the reason.
const EXPECTED: &str = "// refusal expected:";

const TEST_ONLY: &str = "#[cfg(test)]";
const LET_UNDERSCORE: &str = "let _ =";

fn at(code: &[char], i: usize, what: &str) -> bool {
    what.chars().enumerate().all(|(k, c)| code.get(i + k) == Some(&c))
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn after_ident(code: &[char], i: usize) -> bool {
    i.checked_sub(1).and_then(|p| code.get(p)).is_some_and(|p| is_ident(*p))
}

/// Where the comment, string or char literal that starts at `i` ends (`None`: none starts here).
fn end_of_blank(src: &[char], i: usize) -> Option<usize> {
    let n = src.len();
    let c = *src.get(i)?;
    let mut j = i;
    if at(src, i, "//") {
        while j < n && src.get(j) != Some(&'\n') {
            j += 1;
        }
    } else if at(src, i, "/*") {
        let mut depth = 0usize;
        while j < n {
            if at(src, j, "/*") {
                depth += 1;
                j += 2;
            } else if at(src, j, "*/") {
                depth = depth.saturating_sub(1);
                j += 2;
                if depth == 0 {
                    break;
                }
            } else {
                j += 1;
            }
        }
    } else if c == '"' {
        j += 1;
        while j < n {
            match src.get(j) {
                Some('\\') => j += 2,
                Some('"') => {
                    j += 1;
                    break;
                }
                _ => j += 1,
            }
        }
    } else if c == 'r' && (!after_ident(src, i) || (src.get(i.wrapping_sub(1)) == Some(&'b') && !after_ident(src, i.saturating_sub(1)))) {
        // r"…", r#"…"#, br#"…"#: ends at a quote followed by as many hashes
        j += 1;
        while src.get(j) == Some(&'#') {
            j += 1;
        }
        if src.get(j) != Some(&'"') {
            return None;
        }
        let hashes = j - i - 1;
        j += 1;
        while j < n && !(src.get(j) == Some(&'"') && (1..=hashes).all(|k| src.get(j + k) == Some(&'#'))) {
            j += 1;
        }
        j += 1 + hashes;
    } else if c == '\'' && src.get(i + 1) == Some(&'\\') {
        // '\n', '\'', '\u{1F600}'
        j += 3;
        while j < n && src.get(j) != Some(&'\'') && src.get(j) != Some(&'\n') {
            j += 1;
        }
        j += 1;
    } else if c == '\'' && src.get(i + 2) == Some(&'\'') {
        j += 3;
    } else {
        // code (a lone `'` is a lifetime's)
        return None;
    }
    Some(j.min(n))
}

/// The text with comments and string and char literals blanked (newlines kept, so offsets and
/// line numbers are the text's).
fn code_only(text: &str) -> Vec<char> {
    let mut code: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < code.len() {
        let Some(end) = end_of_blank(&code, i) else {
            i += 1;
            continue;
        };
        for c in code.iter_mut().take(end).skip(i) {
            if *c != '\n' {
                *c = ' ';
            }
        }
        i = end.max(i + 1);
    }
    code
}

/// Whether the attribute at `attr` is written inside an open `<…>` (a generic parameter list), looking back
/// to the nearest `<` that nothing before the attribute closed. A `{`, `}` or `;` first, or a bracket that is not
/// matched, means it is not.
fn inside_angle_list(code: &[char], attr: usize) -> bool {
    let (mut angle, mut nested) = (0usize, 0usize);
    for i in (0..attr).rev() {
        let arrow = code.get(i.wrapping_sub(1)).is_some_and(|p| matches!(p, '-' | '='));
        match code[i] {
            ')' | ']' => nested += 1,
            '(' | '[' if nested == 0 => return false,
            '(' | '[' => nested -= 1,
            '{' | '}' | ';' if nested == 0 => return false,
            '>' if !arrow && nested == 0 => angle += 1,
            '<' if nested == 0 && angle == 0 => return true,
            '<' if nested == 0 => angle -= 1,
            _ => {}
        }
    }
    false
}

/// Where what `#[cfg(test)]` applies to ends: `from` is just past the attribute; `in_list`: it is written inside
/// a `<…>` list, whose closing `>` ends it too ([`inside_angle_list`]).
fn end_of_item(code: &[char], from: usize, in_list: bool) -> usize {
    let (mut round, mut square, mut angle) = (0usize, 0usize, 0usize);
    // a `where` clause runs to the item's `{` (or `;`): its commas separate predicates, not items
    let mut in_where = false;
    let mut i = from;
    while let Some(&c) = code.get(i) {
        let outside = round == 0 && square == 0;
        if outside && !in_where && at(code, i, "where") && !after_ident(code, i) && !code.get(i + 5).is_some_and(|c| is_ident(*c)) {
            in_where = true;
        }
        match c {
            '(' => round += 1,
            '[' => square += 1,
            // a bracket that isn't the item's closes what the item is in (a test-only argument)
            ')' if round == 0 => return i,
            ']' if square == 0 => return i,
            ')' => round -= 1,
            ']' => square -= 1,
            '<' => angle += 1,
            // `->` and `=>` are not brackets
            '>' if matches!(i.checked_sub(1).and_then(|p| code.get(p)), Some('-' | '=')) => {}
            // the `>` that closes the generic list the attribute is in (a test-only parameter, the last)
            '>' if in_list && angle == 0 && outside => return i,
            '>' => angle = angle.saturating_sub(1),
            ';' if outside => return i + 1,
            ',' if outside && angle == 0 && !in_where => return i + 1,
            // the last field of a struct: the brace is the struct's
            '}' if outside => return i,
            '{' if outside => {
                let mut depth = 0usize;
                while let Some(&c) = code.get(i) {
                    i += 1;
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                return i;
                            }
                        }
                        _ => {}
                    }
                }
                return i;
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Whether the comment that excuses a dropped `session.execute` is on this line: the marker and
/// a reason after it.
fn says_expected(line: &str, whole_line: bool) -> bool {
    let reason = if whole_line { line.trim_start().strip_prefix(EXPECTED) } else { line.split_once(EXPECTED).map(|(_, reason)| reason) };
    reason.is_some_and(|reason| !reason.trim().is_empty())
}

/// Whether a statement that starts on line index `first` and runs `span` more lines says why a refusal is expected: on one of
/// its lines, or on the line directly above one of them (a call broken over lines can say it where `.execute(` is).
fn says_expected_in(lines: &[&str], first: usize, span: usize) -> bool {
    (first..=first + span).any(|k| {
        lines.get(k).is_some_and(|l| says_expected(l, false)) || k.checked_sub(1).and_then(|p| lines.get(p)).is_some_and(|l| says_expected(l, true))
    })
}

/// The offending lines of one file's text: `(line number, the line)`. Test code (a `tests.rs`, `tests_*.rs` or
/// `*_tests.rs` file, or one in a `tests/` folder, is not passed in; whatever a `#[cfg(test)]` applies to is skipped) may drop what it likes.
pub fn dropped(text: &str) -> Vec<(usize, String)> {
    let code = code_only(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let (mut i, mut line) = (0usize, 1usize);
    while let Some(&c) = code.get(i) {
        // a file that is all test code says so at its head, before any item's body; the same
        // attribute further in (a module's own) is that module's and stops nothing here
        if at(&code, i, "#![cfg(test)]") && !code.iter().take(i).any(|c| *c == '{') {
            return Vec::new();
        }
        if at(&code, i, TEST_ONLY) {
            let end = end_of_item(&code, i + TEST_ONLY.len(), inside_angle_list(&code, i)).max(i + 1);
            line += code.iter().take(end).skip(i).filter(|c| **c == '\n').count();
            i = end;
            continue;
        }
        let after = i + LET_UNDERSCORE.len();
        if at(&code, i, LET_UNDERSCORE) && !after_ident(&code, i) && code.get(after) != Some(&'=') {
            // what is dropped, up to the statement's first `;` (a closure's later statements are
            // their own), white space taken out so a call broken over lines reads as one
            let expression: String = code.iter().skip(after).take_while(|c| **c != ';').filter(|c| !c.is_whitespace()).collect();
            let here = line.checked_sub(1).and_then(|l| lines.get(l)).copied().unwrap_or_default();
            let runs = RUNS.iter().any(|run| expression.contains(run));
            // the statement's lines: a call broken over lines can say why on any of them or above any of them
            let span = code.iter().skip(after).take_while(|c| **c != ';').filter(|c| **c == '\n').count();
            let executes = EXECUTES.iter().any(|run| expression.contains(run)) && !says_expected_in(&lines, line - 1, span);
            if runs || executes {
                found.push((line, here.trim().to_string()));
            }
        }
        if c == '\n' {
            line += 1;
        }
        i += 1;
    }
    found
}

/// A file kept only for test code: `tests.rs`, `tests_*.rs` or `*_tests.rs`.
fn is_test_file_name(stem: &str) -> bool {
    stem == "tests" || stem.starts_with("tests_") || stem.ends_with("_tests")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.is_dir() {
            if !path.file_name().is_some_and(|n| n == "tests") {
                rust_files(&path, out)?;
            }
        } else if path.extension().is_some_and(|e| e == "rs") && !path.file_stem().is_some_and(|n| is_test_file_name(&n.to_string_lossy())) {
            out.push(path);
        }
    }
    Ok(())
}

pub fn run(root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    for dir in DIRS {
        rust_files(&root.join(dir), &mut files)?;
    }
    files.sort();
    let mut problems = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        for (line, code) in dropped(&text) {
            problems.push(format!("{}:{line}: {code}", file.strip_prefix(root).unwrap_or(file).display()));
        }
    }
    if problems.is_empty() {
        println!("OK: no command's refusal is dropped in {} ({} files).", DIRS.join(", "), files.len());
        return Ok(());
    }
    for p in &problems {
        eprintln!("{p}");
    }
    Err(format!(
        "{} command result(s) dropped with `let _ =`: use `app.act(…)` so a refusal is said in a toast, `app.quiet(…)` (and say why) where one is expected, or deal with the `Result`; a `session.execute` that is meant to be dropped says so in a comment on its line or the line above: `{EXPECTED} <reason>`",
        problems.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_result_is_found() {
        let text = "fn ui(app: &mut App) {\n    if clicked {\n        let _ = app.run(\"album.move\", json!({}));\n    }\n}\n";
        assert_eq!(dropped(text), [(3, "let _ = app.run(\"album.move\", json!({}));".to_string())]);
        assert_eq!(dropped("    let _ = self.run(\"x\", p);").len(), 1);
        assert_eq!(dropped("    let _ = lightcraft_ui_egui::menubar::run_item(app, &cmd, params);").len(), 1);
        assert_eq!(dropped("        let _ = app.run(\n            \"photo.setMeta\",\n        );").len(), 1, "a call over several lines");
        assert_eq!(dropped("    let _ = app.run_typed(\"mask.add\", p);").len(), 1, "keeping the kind doesn't keep the refusal");
        assert_eq!(dropped("    let _ = self.run_typed(\"mask.add\", p);").len(), 1);
    }

    #[test]
    fn saying_which_is_meant_passes() {
        for ok in [
            "    app.act(\"album.move\", json!({}));",
            "    app.quiet(\"library.select\", json!({}));",
            "    if let Err(e) = app.run(\"x\", p) { app.toast(ctx, e) }",
            "    let r = app.run(\"x\", p);",
            "    let ok = app.run(id, params).is_ok();",
            "    let _ = std::fs::remove_file(path);",
            "    // let _ = app.run(\"x\", p);",
        ] {
            assert!(dropped(ok).is_empty(), "{ok}");
        }
    }

    #[test]
    fn test_code_is_left_alone() {
        let text = "fn ui() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {\n        let _ = app.run(\"x\", p);\n    }\n}\n";
        assert!(dropped(text).is_empty());
        let before = "fn ui() {\n    let _ = app.run(\"x\", p);\n}\n\n#[cfg(test)]\nmod tests {}\n";
        assert_eq!(dropped(before).len(), 1, "code before the test module still counts");
        assert!(dropped(&format!("#![cfg(test)]\n{DROP}")).is_empty(), "a file that is all test code");
        let inner = format!("{DROP}mod t {{\n    #![cfg(test)]\n}}\n{DROP}");
        assert_eq!(dropped(&inner).len(), 2, "a module's own #![cfg(test)] does not excuse the file");
    }

    const DROP: &str = "fn ui(app: &mut App) {\n    let _ = app.run(\"x\", p);\n}\n";

    fn lines(text: &str) -> Vec<usize> {
        dropped(text).into_iter().map(|(line, _)| line).collect()
    }

    #[test]
    fn a_test_module_declared_elsewhere_skips_only_itself() {
        // panels/grid.rs: the attribute, a #[path], the declaration; then the whole file
        let text = format!("//! Grid.\n\n#[cfg(test)]\n#[path = \"../tests_grid.rs\"]\nmod tests_grid;\n\n{DROP}");
        assert_eq!(lines(&text), [8]);
        // lib.rs: a run of them
        let text = format!("#[cfg(test)]\nmod tests_activity;\n#[cfg(test)]\nmod tests_scroll;\n{DROP}");
        assert_eq!(lines(&text), [6]);
        // on one line
        let text = format!("#[cfg(test)] mod tests_a;\n{DROP}");
        assert_eq!(lines(&text), [3]);
    }

    #[test]
    fn code_after_a_test_module_still_counts() {
        // panels/detail.rs: production code below a test module
        let text = format!("#[cfg(test)]\nmod tests {{\n    fn t() {{\n        let _ = app.run(\"x\", p);\n    }}\n}}\n\n{DROP}");
        assert_eq!(lines(&text), [9]);
    }

    #[test]
    fn a_test_only_field_or_fn_skips_only_itself() {
        // render.rs: a field of a struct, with generics and a comma of its own
        let text = format!("struct R {{\n    #[cfg(test)]\n    pub seen: HashMap<u32, Vec<u8>>,\n    pub n: [u8; 3],\n}}\n{DROP}");
        assert_eq!(lines(&text), [7]);
        // the last field, without a comma: the struct's brace isn't the field's
        let text = format!("struct R {{\n    #[cfg(test)]\n    seen: u32\n}}\n{DROP}");
        assert_eq!(lines(&text), [6]);
        // headless.rs: a fn inside an impl; what it drops is a test's business, what follows isn't
        let text = "impl H {\n    #[cfg(test)]\n    pub fn poke<T: Into<u8>>(&mut self, n: [u8; 2]) -> u8 {\n        let _ = self.run(\"x\", p);\n        0\n    }\n    fn real(&mut self) {\n        let _ = self.run(\"y\", p);\n    }\n}\n";
        assert_eq!(lines(text), [8]);
        // a statement
        let text = "fn f(app: &mut App) {\n    #[cfg(test)]\n    let _ = app.run(\"x\", p);\n    let _ = app.run(\"y\", p);\n}\n";
        assert_eq!(lines(text), [4]);
        // a `where` clause: its commas separate predicates, they don't end the fn
        let text = "#[cfg(test)]\nfn poke<T>(app: &mut App)\nwhere\n    T: Into<u8>,\n    T: Copy,\n{\n    let _ = app.run(\"x\", p);\n}\nfn real(app: &mut App) {\n    let _ = app.run(\"y\", p);\n}\n";
        assert_eq!(lines(text), [10]);
        // a test-only generic parameter, the last of its list: the `>` is the list's, so the `where` after it and
        // the production body below are not the parameter's
        let text =
            "fn ui<U, #[cfg(test)] T>(app: &mut App, p: Params, #[cfg(test)] _extra: T)\nwhere\n    U: Copy,\n{\n    let _ = app.run(\"x\", p);\n}\n";
        assert_eq!(lines(text), [5]);
        // not the last: its comma ends it, as for any other item
        let text = "fn ui<#[cfg(test)] T, U>(app: &mut App)\nwhere\n    U: Copy,\n{\n    let _ = app.run(\"x\", p);\n}\n";
        assert_eq!(lines(text), [5]);
        // a bracket pair before it in the list doesn't hide the list
        let text = "fn ui<F: Fn(u8), #[cfg(test)] T>(app: &mut App)\nwhere\n    F: Copy,\n{\n    let _ = app.run(\"x\", p);\n}\n";
        assert_eq!(lines(text), [5]);
        // a `>` that is a comparison is not a list's end: the test-only `if` is skipped whole
        let text = "fn f(app: &mut App, n: u8) {\n    #[cfg(test)]\n    if n > 0 {\n        let _ = app.run(\"x\", p);\n    }\n    let _ = app.run(\"y\", p);\n}\n";
        assert_eq!(lines(text), [6]);
    }

    #[test]
    fn braces_in_strings_chars_and_comments_dont_count() {
        let text = format!(
            "#[cfg(test)]\nmod tests {{\n    const A: &str = \"}}}} \\\" }}\";\n    const B: char = '}}';\n    const C: &str = r#\"}} \" }}\"#;\n    // }}\n    /* }} /* }} */ }} */\n    fn t<'a>(x: &'a str, q: char) -> bool {{\n        let _ = app.run(\"x\", p);\n        q == '\\''\n    }}\n}}\n{DROP}"
        );
        assert_eq!(lines(&text), [14]);
        // and what looks like a drop inside a string or a comment isn't one
        assert!(dropped("    let s = \"let _ = app.run(x);\";\n    /* let _ = app.run(y); */\n").is_empty());
    }

    #[test]
    fn a_call_that_begins_on_the_next_line_is_found() {
        let text = "fn f() {\n    let _ =\n        app.run(\"photo.setMeta\", json!({\"id\": id, \"title\": title}));\n}\n";
        assert_eq!(dropped(text), [(2, "let _ =".to_string())]);
        assert_eq!(lines("    let _ = app\n        .run(\"x\", p);\n"), [1], "broken before the dot");
        assert_eq!(lines("    let _ = app\n        .session\n        .execute(\"x\", &p);\n"), [1]);
        // the statement ends at its semicolon: the next one is its own
        assert!(dropped("    let _ = tx.send(1);\n    app.run(\"x\", p)?;\n").is_empty());
        assert!(dropped("    let _unused = 1;\n    let r = app.run(\"x\", p);\n").is_empty());
    }

    #[test]
    fn going_round_run_is_found_too() {
        assert_eq!(lines("    let _ = app.session.execute(\"x\", &p);"), [1]);
        assert_eq!(lines("    let _ = self.session.execute(\"x\", &p);"), [1]);
        assert!(dropped("    let r = app.session.execute(\"x\", &p);").is_empty());
        assert!(dropped("    app.session.execute(\"x\", &p)?;").is_empty());
    }

    #[test]
    fn an_expected_refusal_says_so_with_a_reason() {
        let above = "    // refusal expected: the task may have finished since the row was drawn\n    let _ = app.session.execute(\"activity.cancel\", &p);\n";
        assert!(dropped(above).is_empty());
        let same = "    let _ = app.session.execute(\"x\", &p); // refusal expected: applied at start-up, nobody asked\n";
        assert!(dropped(same).is_empty());
        // a reason is wanted, and the comment has to be the line directly above
        assert_eq!(lines("    // refusal expected:\n    let _ = app.session.execute(\"x\", &p);\n"), [2]);
        assert_eq!(lines("    // refusal expected\n    let _ = app.session.execute(\"x\", &p);\n"), [2]);
        assert_eq!(lines("    // refusal expected: yes\n\n    let _ = app.session.execute(\"x\", &p);\n"), [3]);
        assert_eq!(lines("    // it may be refused\n    let _ = app.session.execute(\"x\", &p);\n"), [2]);
        // `run` has `quiet` for that: no comment excuses it
        assert_eq!(lines("    // refusal expected: because\n    let _ = app.run(\"x\", p);\n"), [2]);
        // a call broken over lines says it where `.execute(` is, or above it
        assert!(dropped("    let _ = app\n        .session\n        // refusal expected: gone by itself\n        .execute(\"x\", &p);\n").is_empty());
        assert!(dropped("    let _ = app\n        .session\n        .execute(\"x\", &p); // refusal expected: gone by itself\n").is_empty());
        assert_eq!(lines("    let _ = app\n        .session\n        // refusal expected:\n        .execute(\"x\", &p);\n"), [1]);
    }

    #[test]
    fn the_interface_drops_none() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        run(root).unwrap();
    }
}
