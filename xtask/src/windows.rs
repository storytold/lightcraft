//! `cargo xtask windows` (a step of `ci`): every window `crates/ui-egui/src` draws with
//! `egui::Window::new` is on the list in `crates/ui-egui/windows.json`, as a modal (it takes the keyboard
//! and the pointer while it is up) or as an exception with its reason, and every entry there still has its
//! window. It reads the source only: a window is found by its file and the function it is drawn in.
//!
//! Own design.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const CRATE: &str = "crates/ui-egui";
const LIST: &str = "crates/ui-egui/windows.json";

/// One `Window::new` in the source.
#[derive(Debug, PartialEq)]
pub struct Site {
    /// Relative to the crate (`src/edit_in.rs`), as the list writes it.
    pub file: String,
    pub line: usize,
    pub function: Option<String>,
}

/// One entry of the list.
#[derive(Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub file: String,
    pub function: String,
    /// `Id::new` of the first, then `.with` of each other.
    pub id: Vec<String>,
    /// How many `Window::new` the function has (`"count"`, 1 when left out), so a second window
    /// added to a listed function is found too.
    pub count: usize,
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    match args {
        [] | ["--check"] => check(root),
        _ => Err("usage: cargo xtask windows [--check]".into()),
    }
}

pub fn check(root: &Path) -> Result<(), String> {
    let list = std::fs::read_to_string(root.join(LIST)).map_err(|e| format!("windows: can't read {LIST}: {e}"))?;
    let entries = parse_list(&list)?;
    let mut sites = Vec::new();
    let mut texts = BTreeMap::new();
    for file in rust_files(&root.join(CRATE).join("src"))? {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("windows: can't read {}: {e}", file.display()))?;
        let rel = file.strip_prefix(root.join(CRATE)).unwrap_or(&file).to_string_lossy().replace('\\', "/");
        sites.extend(scan(&rel, &text));
        texts.insert(rel, text);
    }
    let problems = problems(&sites, &entries, &texts);
    if problems.is_empty() {
        println!("windows: OK ({} windows, each listed in {LIST})", sites.len());
        return Ok(());
    }
    for p in &problems {
        eprintln!("{p}");
    }
    Err(format!(
        "windows: {} problem(s). Every `egui::Window::new` in {CRATE}/src is listed in {LIST} by its file and function: \
         a modal (counted in `keyboard::modal_open`, holding egui's modal layer, with a row in the survey test \
         `tests_dialogs.rs`) or an exception with its reason (AGENTS.md)",
        problems.len()
    ))
}

/// The list's entries, checked for what each kind needs.
pub fn parse_list(text: &str) -> Result<Vec<Entry>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("windows: {LIST} isn't valid JSON: {e}"))?;
    let list = v.get("windows").and_then(serde_json::Value::as_array).ok_or_else(|| format!("windows: {LIST} has no \"windows\" array"))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, w) in list.iter().enumerate() {
        let s = |k: &str| w.get(k).and_then(serde_json::Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let at = format!("{LIST}: entry {}", i + 1);
        let name = s("name").ok_or_else(|| format!("{at} has no name"))?;
        let at = format!("{at} ({name})");
        let file = s("file").ok_or_else(|| format!("{at} has no file"))?;
        let function = s("function").ok_or_else(|| format!("{at} has no function"))?;
        match s("kind") {
            Some("modal") => {}
            Some("exception") if s("reason").is_some() => {}
            Some("exception") => return Err(format!("{at}: an exception says why in \"reason\"")),
            _ => return Err(format!("{at}: \"kind\" is \"modal\" or \"exception\"")),
        }
        let id = match w.get("id") {
            None => Vec::new(),
            Some(serde_json::Value::Array(a)) if !a.is_empty() => a
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| format!("{at}: \"id\" is a list of strings"))?,
            Some(_) => return Err(format!("{at}: \"id\" is a list of strings")),
        };
        let count = match w.get("count") {
            None => 1,
            Some(c) => {
                c.as_u64().filter(|&c| c > 1).and_then(|c| usize::try_from(c).ok()).ok_or_else(|| format!("{at}: \"count\" is a number above 1"))?
            }
        };
        if !seen.insert((file.to_string(), function.to_string())) {
            return Err(format!("{at}: {file} `{function}` is listed twice"));
        }
        out.push(Entry { name: name.to_string(), file: file.to_string(), function: function.to_string(), id, count });
    }
    Ok(out)
}

/// The windows in one file: each `Window::new` outside a comment, with the function it is drawn in
/// (the last `fn` before it).
pub fn scan(file: &str, text: &str) -> Vec<Site> {
    let mut function = None;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        if let Some(name) = fn_name(code) {
            function = Some(name);
        }
        if code.contains("Window::new(") {
            out.push(Site { file: file.to_string(), line: i + 1, function: function.clone() });
        }
    }
    out
}

/// `foo` out of a line declaring `fn foo(` or `fn foo<`; not a function-pointer type (`fn(&mut Ui)`).
fn fn_name(code: &str) -> Option<String> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut rest = code;
    while let Some(at) = rest.find("fn ") {
        let before = rest[..at].chars().next_back();
        let after = rest[at + 3..].trim_start();
        rest = &rest[at + 3..];
        if before.is_some_and(ident) {
            continue;
        }
        let len = after.find(|c: char| !ident(c)).unwrap_or(after.len());
        let (name, tail) = after.split_at(len);
        if name.starts_with(|c: char| c.is_alphabetic() || c == '_') && (tail.starts_with('(') || tail.starts_with('<')) {
            return Some(name.to_string());
        }
    }
    None
}

/// What is wrong: windows not on the list, entries with no window or more windows than their count,
/// ids the file doesn't spell.
pub fn problems(sites: &[Site], entries: &[Entry], texts: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    let listed: BTreeSet<(&str, &str)> = entries.iter().map(|e| (e.file.as_str(), e.function.as_str())).collect();
    let mut found: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for s in sites {
        let Some(function) = &s.function else {
            out.push(format!("{CRATE}/{}:{}: a window outside any function: the list names a window by the function that draws it", s.file, s.line));
            continue;
        };
        found.entry((s.file.as_str(), function.as_str())).or_default().push(s.line);
        if !listed.contains(&(s.file.as_str(), function.as_str())) {
            out.push(format!(
                "{CRATE}/{}:{}: the window in `{function}` isn't in {LIST}: add it as a modal or an exception with its reason",
                s.file, s.line
            ));
        }
    }
    for e in entries {
        let Some(lines) = found.get(&(e.file.as_str(), e.function.as_str())) else {
            out.push(format!(
                "{LIST}: \"{}\": {} `{}` draws no window: remove the entry, or name the file and function it moved to",
                e.name, e.file, e.function
            ));
            continue;
        };
        if lines.len() != e.count {
            let at = lines.iter().map(|l| format!("{CRATE}/{}:{l}", e.file)).collect::<Vec<_>>().join(", ");
            out.push(format!(
                "{LIST}: \"{}\": `{}` has {} windows ({at}) where the list counts {}: list a new one as a modal or an exception \
                 (its own entry, or \"count\" when it is the same kind)",
                e.name,
                e.function,
                lines.len(),
                e.count
            ));
        }
        let text = texts.get(&e.file).map(String::as_str).unwrap_or_default();
        for part in &e.id {
            if !text.contains(&format!("\"{part}\"")) {
                out.push(format!("{LIST}: \"{}\": its id part \"{part}\" isn't written in {}: correct the id or leave it out", e.name, e.file));
            }
        }
    }
    out
}

/// Every `.rs` file under `dir`, sorted, but the UI tests' files (`tests_*.rs`): a test may draw a
/// window of its own.
fn rust_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let read = std::fs::read_dir(&d).map_err(|e| format!("windows: can't read {}: {e}", d.display()))?;
        for entry in read {
            let path = entry.map_err(|e| format!("windows: can't read {}: {e}", d.display()))?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") && !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("tests_")) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, function: &str, id: &[&str]) -> Entry {
        Entry { name: function.into(), file: file.into(), function: function.into(), id: id.iter().map(|s| s.to_string()).collect(), count: 1 }
    }

    #[test]
    fn a_window_is_found_by_its_function_and_comments_are_skipped() {
        let text = "//! let shown = egui::Window::new(title).show(ctx, |ui| {});\nfn first() {}\npub(crate) fn roles_window(app: &mut A) {\n    egui::Window::new(tr(\"Roles\")) // egui::Window::new( in a comment\n}\n";
        assert_eq!(scan("src/m.rs", text), vec![Site { file: "src/m.rs".into(), line: 4, function: Some("roles_window".into()) }]);
    }

    #[test]
    fn a_function_pointer_type_isnt_a_function() {
        let text = "fn show(ctx: &Context) {\n    let close: fn(&mut Ui) = |_| {};\n    type F = fn(&mut X);\n    egui::Window::new(\"a\");\n}\nfn generic<T>() { egui::Window::new(\"b\"); }\n";
        let at: Vec<_> = scan("src/m.rs", text).into_iter().map(|s| s.function).collect();
        assert_eq!(at, vec![Some("show".into()), Some("generic".into())]);
    }

    #[test]
    fn a_second_window_in_a_listed_function_is_a_problem() {
        let site = |line| Site { file: "src/a.rs".into(), line, function: Some("show".into()) };
        let texts = BTreeMap::from([("src/a.rs".to_string(), String::new())]);
        let p = problems(&[site(3), site(9)], &[entry("src/a.rs", "show", &[])], &texts);
        assert!(p.len() == 1 && p[0].contains("2 windows") && p[0].contains("src/a.rs:9"), "{p:?}");
        let two = Entry { count: 2, ..entry("src/a.rs", "show", &[]) };
        assert!(problems(&[site(3), site(9)], &[two], &texts).is_empty());
    }

    #[test]
    fn an_unlisted_window_and_a_stale_entry_are_both_problems() {
        let sites = vec![Site { file: "src/a.rs".into(), line: 3, function: Some("new_one".into()) }];
        let texts = BTreeMap::from([("src/a.rs".to_string(), String::new())]);
        let p = problems(&sites, &[entry("src/a.rs", "gone", &[])], &texts);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(p[0].contains("src/a.rs:3") && p[0].contains("`new_one`") && p[0].contains("modal or an exception"), "{}", p[0]);
        assert!(p[1].contains("`gone` draws no window"), "{}", p[1]);
    }

    #[test]
    fn a_listed_window_passes_and_its_id_is_checked_against_the_file() {
        let sites = vec![Site { file: "src/a.rs".into(), line: 3, function: Some("show".into()) }];
        let texts = BTreeMap::from([("src/a.rs".to_string(), "egui::Id::new(\"here\")".to_string())]);
        assert!(problems(&sites, &[entry("src/a.rs", "show", &["here"])], &texts).is_empty());
        let p = problems(&sites, &[entry("src/a.rs", "show", &["here", "elsewhere"])], &texts);
        assert!(p.len() == 1 && p[0].contains("\"elsewhere\""), "{p:?}");
    }

    #[test]
    fn a_bad_list_is_an_error_not_a_panic() {
        for (text, says) in [
            ("not json", "isn't valid JSON"),
            ("{}", "no \"windows\" array"),
            (r#"{"windows": [{"name": "W", "file": "f", "function": "g", "kind": "exception"}]}"#, "says why"),
            (r#"{"windows": [{"name": "W", "file": "f", "function": "g", "kind": "floating"}]}"#, "\"kind\" is"),
            (r#"{"windows": [{"name": "W", "file": "f", "function": "g", "kind": "modal", "id": "x"}]}"#, "list of strings"),
            (r#"{"windows": [{"name": "W", "function": "g", "kind": "modal"}]}"#, "has no file"),
            (
                r#"{"windows": [{"name": "W", "file": "f", "function": "g", "kind": "modal"}, {"name": "V", "file": "f", "function": "g", "kind": "modal"}]}"#,
                "listed twice",
            ),
            (r#"{"windows": [{"name": "W", "file": "f", "function": "g", "kind": "modal", "count": 1}]}"#, "above 1"),
        ] {
            let e = parse_list(text).expect_err(text);
            assert!(e.contains(says), "{text}: {e}");
        }
    }

    /// The list in the repo matches the source (what the `ci` step checks).
    #[test]
    fn the_repos_list_matches_the_ui_source() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        check(&root).unwrap();
    }
}
