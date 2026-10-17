//! Release notes, one file per change, so two pull requests never edit the same file for them.
//!
//! - **What's New** (Help ▸ What's New): each entry is `docs/whats-new/<YYYY-MM-DD>-<slug>.md`, one `### Heading`
//!   and its lines. `docs/whats-new.md` holds upstream's entries only. [`join_whats_new`] puts both together, the
//!   newest date first, at build time (`crates/ui-egui/build.rs`); nothing joined is committed.
//! - **Changelog:** each entry is `docs/changelog/unreleased/<YYYY-MM-DD>-<slug>.md`, one `## Section` and its
//!   bullets. At release, [`release_changelog`] gathers them into `docs/changelog/<version>.md`
//!   (`cargo xtask notes --release <version>`).
//!
//! The date in a name is the day the entry was written; it places a What's New entry under its date and orders
//! entries within a changelog section. Errors name the file and what is wrong; nothing here panics.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

/// Files in an entry folder that are not entries: its own explanation, and (What's New) the hash of upstream's
/// file recorded at the last upstream merge.
pub const NOT_ENTRIES: &[&str] = &["README.md", UPSTREAM_STAMP];
/// The hash of `docs/whats-new.md` recorded at the last upstream merge, in `docs/whats-new/`.
pub const UPSTREAM_STAMP: &str = "upstream.sha256";
/// The changelog's sections in the order releases list them; a section not named here goes after these.
pub const SECTION_ORDER: &[&str] = &[
    "Highlights",
    "Develop and masking",
    "Develop",
    "Library",
    "Import, export and formats",
    "Import",
    "Display",
    "Interface",
    "Performance",
    "Updater and security",
    "Release builds and CI",
    "From upstream",
    "Upstream merge",
    "Fixes",
    "Development",
];
/// The title of the joined What's New when upstream's file has none.
const DEFAULT_TITLE: &str = "# What's new in {app}";

/// What is wrong with one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The file's name (or path, from [`read_entries`]).
    pub file: String,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.file, self.message)
    }
}

impl std::error::Error for Error {}

/// Which kind of entry a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `docs/whats-new/`: starts with `### Heading`.
    WhatsNew,
    /// `docs/changelog/unreleased/`: starts with `## Section`.
    Changelog,
}

impl Kind {
    fn marker(self) -> &'static str {
        match self {
            Kind::WhatsNew => "### ",
            Kind::Changelog => "## ",
        }
    }
}

/// One checked entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The file name, `YYYY-MM-DD-<slug>.md`.
    pub name: String,
    /// `YYYY-MM-DD`, from the name.
    pub date: String,
    /// The heading's text: the What's New title, or the changelog section.
    pub heading: String,
    /// The lines under the heading, without blank lines at either end; every line ends in `\n`.
    pub body: String,
}

fn error(file: &str, message: impl Into<String>) -> Error {
    Error { file: file.to_string(), message: message.into() }
}

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `YYYY-MM-DD` if it is a real calendar date.
pub fn valid_date(date: &str) -> bool {
    let mut parts = date.split('-');
    let (Some(y), Some(m), Some(d), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return false;
    }
    let (Some(y), Some(m), Some(d)) = (digits(y), digits(m), digits(d)) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

/// The date of an entry's file name, `YYYY-MM-DD-<slug>.md`, where the slug is lowercase letters, digits and
/// single hyphens.
pub fn entry_date(name: &str) -> Result<String, String> {
    let shape = "name it `YYYY-MM-DD-<slug>.md` (the date it was written; the slug in lowercase letters, digits \
                 and hyphens)";
    let Some(stem) = name.strip_suffix(".md") else {
        return Err(format!("not a `.md` file: {shape}"));
    };
    let (Some(date), Some(dash), Some(slug)) = (stem.get(..10), stem.get(10..11), stem.get(11..)) else {
        return Err(format!("no date and slug: {shape}"));
    };
    if !valid_date(date) {
        return Err(format!("`{date}` is not a date: {shape}"));
    }
    let slug_ok = dash == "-"
        && !slug.is_empty()
        && slug.split('-').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    if !slug_ok {
        return Err(format!("bad slug `{slug}`: {shape}"));
    }
    Ok(date.to_string())
}

/// Lines without a byte-order mark or `\r`.
fn clean(text: &str) -> String {
    text.strip_prefix('\u{feff}').unwrap_or(text).replace("\r\n", "\n")
}

/// The lines between the first and last non-blank one, each ending in `\n` and otherwise as written.
fn trimmed_block(lines: &[&str]) -> String {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let end = lines.iter().rposition(|l| !l.trim().is_empty());
    match (start, end) {
        (Some(s), Some(e)) => lines.get(s..=e).unwrap_or_default().iter().map(|l| format!("{l}\n")).collect(),
        _ => String::new(),
    }
}

/// A heading line's level: `# x` → 1, `## x` → 2, `### x` → 3; anything else (`#hashtag`, text) → None.
fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    (hashes > 0 && line.get(hashes..hashes + 1) == Some(" ")).then_some(hashes)
}

/// Each line's heading level, `None` inside a fenced code block (a `# comment` in a shell snippet is not a heading).
fn heading_levels<'a>(lines: impl IntoIterator<Item = &'a str>) -> impl Iterator<Item = (&'a str, Option<usize>)> {
    let mut fence: Option<&str> = None;
    lines.into_iter().map(move |line| {
        let trimmed = line.trim_start();
        if let Some(mark) = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m)) {
            match fence {
                None => fence = Some(mark),
                Some(open) if open == mark => fence = None,
                Some(_) => {}
            }
            return (line, None);
        }
        (line, if fence.is_some() { None } else { heading_level(line) })
    })
}

/// Checks one entry file's name and text.
pub fn parse_entry(kind: Kind, name: &str, text: &str) -> Result<Entry, Error> {
    let date = entry_date(name).map_err(|m| error(name, m))?;
    let text = clean(text);
    let lines: Vec<&str> = text.lines().collect();
    let Some(first) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return Err(error(name, "empty"));
    };
    let head = lines.get(first).copied().unwrap_or_default().trim_end();
    let want = kind.marker();
    let what = match kind {
        Kind::WhatsNew => "a What's New entry starts with `### <title>`",
        Kind::Changelog => "a changelog entry starts with `## <section>` (e.g. `## Fixes`)",
    };
    let Some(heading) = head.strip_prefix(want).map(str::trim).filter(|h| !h.is_empty()) else {
        return Err(error(name, format!("line {}: {what}", first + 1)));
    };
    let rest = lines.get(first + 1..).unwrap_or_default();
    for (i, (_, level)) in heading_levels(rest.iter().copied()).enumerate() {
        let n = first + 2 + i;
        let bad = match (kind, level) {
            (Kind::WhatsNew, Some(1 | 2)) => Some("a `#` or `##` heading: the date comes from the file name"),
            (Kind::WhatsNew, Some(3)) => Some("a second `###` heading: one entry per file"),
            (Kind::Changelog, Some(1)) => Some("a `#` heading: the release's title comes from `--release`"),
            (Kind::Changelog, Some(2)) => Some("a second `##` section: one section per file"),
            _ => None,
        };
        if let Some(bad) = bad {
            return Err(error(name, format!("line {n}: {bad}")));
        }
    }
    let body = trimmed_block(rest);
    if body.is_empty() {
        return Err(error(name, "nothing under the heading"));
    }
    Ok(Entry { name: name.to_string(), date, heading: heading.to_string(), body })
}

/// Reads and checks every entry in `dir`, oldest first (by date, then name). A missing folder has none.
/// Hidden files (`.DS_Store`) and [`NOT_ENTRIES`] are skipped; anything else that isn't an entry is an error.
pub fn read_entries(kind: Kind, dir: &Path) -> Result<Vec<Entry>, Vec<Error>> {
    let shown = dir.display().to_string();
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(vec![error(&shown, e.to_string())]),
    };
    let (mut entries, mut errors) = (Vec::new(), Vec::new());
    for item in listing {
        let item = match item {
            Ok(item) => item,
            Err(e) => {
                errors.push(error(&shown, e.to_string()));
                continue;
            }
        };
        let path = item.path();
        let file = path.display().to_string();
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            errors.push(error(&file, "the name is not UTF-8"));
            continue;
        };
        if name.starts_with('.') || NOT_ENTRIES.contains(&name.as_str()) {
            continue;
        }
        if path.is_dir() {
            errors.push(error(&file, "a folder: entries are files directly in this folder"));
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_entry(kind, &name, &text) {
                Ok(entry) => entries.push(entry),
                Err(e) => errors.push(error(&file, e.message)),
            },
            Err(e) => errors.push(error(&file, e.to_string())),
        }
    }
    if !errors.is_empty() {
        errors.sort_by(|a, b| a.file.cmp(&b.file));
        return Err(errors);
    }
    entries.sort_by(|a, b| (&a.date, &a.name).cmp(&(&b.date, &b.name)));
    Ok(entries)
}

/// A Markdown file cut at its `## ` headings: the text before the first one, then each heading's text and lines.
struct Sections<'a> {
    preamble: Vec<&'a str>,
    sections: Vec<(String, Vec<&'a str>)>,
}

fn sections(text: &str) -> Sections<'_> {
    let mut out = Sections { preamble: Vec::new(), sections: Vec::new() };
    for (line, level) in heading_levels(text.lines()) {
        if level == Some(2) {
            let title = line.get(3..).unwrap_or_default().trim().to_string();
            out.sections.push((title, Vec::new()));
        } else if let Some((_, lines)) = out.sections.last_mut() {
            lines.push(line);
        } else {
            out.preamble.push(line);
        }
    }
    out
}

/// What's New as the app shows it: upstream's file (`docs/whats-new.md`) with the entries of `docs/whats-new/`
/// added. Dates run newest first; on a date both have, one `## date` section holds the entries first (by file
/// name) and then upstream's text for that date, unchanged. Upstream's title is kept, and a `##` section of its
/// that doesn't start with a date comes after the dates, in upstream's order.
pub fn join_whats_new(upstream: &str, entries: &[Entry]) -> String {
    let upstream = clean(upstream);
    let parsed = sections(&upstream);
    let title = trimmed_block(&parsed.preamble);
    let mut out = if title.is_empty() { format!("{DEFAULT_TITLE}\n") } else { title };
    let (dated, undated): (Vec<_>, Vec<_>) = parsed.sections.iter().partition(|(d, _)| d.get(..10).is_some_and(valid_date));
    let dates: BTreeSet<&str> = dated.iter().map(|(d, _)| d.as_str()).chain(entries.iter().map(|e| e.date.as_str())).collect();
    for date in dates.into_iter().rev() {
        out.push_str(&format!("\n## {date}\n"));
        let mut ours: Vec<&Entry> = entries.iter().filter(|e| e.date == date).collect();
        ours.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in ours {
            out.push_str(&format!("\n### {}\n{}", entry.heading, entry.body));
        }
        for (_, lines) in dated.iter().filter(|(d, _)| d == date) {
            push_block(&mut out, lines);
        }
    }
    for (title, lines) in undated {
        out.push_str(&format!("\n## {title}\n"));
        push_block(&mut out, lines);
    }
    out
}

/// Adds a section's lines, without blank ends, after a blank line; nothing when they're all blank.
fn push_block(out: &mut String, lines: &[&str]) {
    let block = trimmed_block(lines);
    if !block.is_empty() {
        out.push('\n');
        out.push_str(&block);
    }
}

/// The order a changelog section goes in: [`SECTION_ORDER`] first, then the others in the order they first appear.
fn section_rank(section: &str, unknown: &[String]) -> usize {
    SECTION_ORDER
        .iter()
        .position(|s| *s == section)
        .unwrap_or_else(|| SECTION_ORDER.len() + unknown.iter().position(|s| s == section).unwrap_or(unknown.len()))
}

/// `docs/changelog/<version>.md` with the changelog `entries` gathered in. Each goes under its `## Section`, after
/// any entries already there, in date order.
/// - `existing`: the file as already started by hand, kept as written; sections it lacks are added at its end.
/// - None: a new file titled `# <version>` with the sections in [`SECTION_ORDER`] (others after).
pub fn release_changelog(existing: Option<&str>, version: &str, entries: &[Entry]) -> String {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| (&a.date, &a.name).cmp(&(&b.date, &b.name)));
    let mut unknown: Vec<String> = Vec::new();
    for e in &sorted {
        if !SECTION_ORDER.contains(&e.heading.as_str()) && !unknown.contains(&e.heading) {
            unknown.push(e.heading.clone());
        }
    }
    let existing = existing.map(clean);
    let parsed = existing.as_deref().map(sections);
    let mut out = match &parsed {
        Some(p) if !trimmed_block(&p.preamble).is_empty() => trimmed_block(&p.preamble),
        _ => format!("# {version}\n"),
    };
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let bodies = |section: &str| -> String { sorted.iter().filter(|e| e.heading == section).map(|e| e.body.as_str()).collect() };
    if let Some(p) = &parsed {
        for (title, lines) in &p.sections {
            out.push_str(&format!("\n## {title}\n"));
            let block = trimmed_block(lines);
            let added = if done.insert(title.as_str()) { bodies(title) } else { String::new() };
            if !block.is_empty() || !added.is_empty() {
                out.push('\n');
            }
            out.push_str(&block);
            // a hand-written paragraph ends before the gathered bullets start
            if block.lines().last().is_some_and(|l| !l.starts_with(['-', '*', ' '])) && !added.is_empty() {
                out.push('\n');
            }
            out.push_str(&added);
        }
    }
    let mut new_sections: Vec<&str> = sorted.iter().map(|e| e.heading.as_str()).filter(|h| !done.contains(h)).collect();
    new_sections.sort_by_key(|s| section_rank(s, &unknown));
    new_sections.dedup();
    for section in new_sections {
        out.push_str(&format!("\n## {section}\n\n{}", bodies(section)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, name: &str, text: &str) -> Entry {
        parse_entry(kind, name, text).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn names_carry_a_real_date_and_a_plain_slug() {
        for good in ["2026-10-16-release-notes.md", "2024-02-29-leap.md", "2026-01-31-a.md", "2026-10-16-v2-fix.md"] {
            assert!(entry_date(good).is_ok(), "{good} should be accepted");
        }
        for bad in [
            "2026-10-16-release-notes.txt",
            "2026-10-16.md",
            "2026-10-16-.md",
            "2026-10-16_notes.md",
            "2023-02-29-not-leap.md",
            "2026-13-01-month.md",
            "2026-04-31-day.md",
            "2026-00-10-zero.md",
            "26-10-16-short.md",
            "2026-10-16-Capital.md",
            "2026-10-16-two--dashes.md",
            "2026-10-16-trailing-.md",
            "2026-10-16-spa ce.md",
            "notes.md",
            "README.md",
        ] {
            assert!(entry_date(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn hostile_names_and_texts_are_errors_not_panics() {
        // multi-byte characters around the byte offsets the name is cut at
        for name in ["2026-10-1é-x.md", "2026-10-16é-x.md", "é", "", ".md", "2026-10-16-\u{0}.md", "２０２６-10-16-x.md"] {
            assert!(entry_date(name).is_err(), "{name:?} should be refused");
        }
        for text in ["", "\n\n", "###", "### ", "#", "##\n", "\u{feff}", "\r\n\r\n", "### é\n", "#é x\n- y"] {
            for kind in [Kind::WhatsNew, Kind::Changelog] {
                let _ = parse_entry(kind, "2026-10-16-x.md", text);
            }
        }
        let _ = join_whats_new("## \n## 2026\n#\n", &[]);
        let _ = release_changelog(Some("## \n##\n"), "", &[]);
    }

    #[test]
    fn a_whats_new_entry_is_one_titled_block() {
        let e = entry(Kind::WhatsNew, "2026-10-16-x.md", "\n### Faster grid  \n- It scrolls.\n  Smoothly.\n\n- Twice.\n\n");
        assert_eq!(e.date, "2026-10-16");
        assert_eq!(e.heading, "Faster grid");
        assert_eq!(e.body, "- It scrolls.\n  Smoothly.\n\n- Twice.\n", "the body keeps its lines, without blank ends");
        let crlf = entry(Kind::WhatsNew, "2026-10-16-x.md", "\u{feff}### A\r\n- b\r\n");
        assert_eq!((crlf.heading.as_str(), crlf.body.as_str()), ("A", "- b\n"), "a BOM and CRLF are read as plain lines");
        let refused = [
            ("", "empty"),
            ("- no heading\n", "starts with `### <title>`"),
            ("## 2026-10-16\n### A\n- b\n", "starts with `### <title>`"),
            ("### A\n## 2026-10-16\n- b\n", "line 2: a `#` or `##` heading"),
            ("### A\n- b\n# Title\n", "line 3: a `#` or `##` heading"),
            ("### A\n- b\n### B\n- c\n", "line 3: a second `###` heading"),
            ("### A\n\n\n", "nothing under the heading"),
            ("###A\n- b\n", "starts with `### <title>`"),
        ];
        for (text, want) in refused {
            let e = parse_entry(Kind::WhatsNew, "2026-10-16-x.md", text).err().unwrap_or_else(|| panic!("{text:?} should be refused"));
            assert!(e.message.contains(want), "{text:?}: {} should say {want}", e.message);
            assert_eq!(e.file, "2026-10-16-x.md", "the error names the file");
        }
        assert!(parse_entry(Kind::WhatsNew, "2026-10-16-x.md", "### A\n- #hashtag and `## code`\n").is_ok(), "only real headings count");
        let fenced = "### A\nRun:\n```sh\n# a comment\n## another\n```\n";
        assert!(parse_entry(Kind::WhatsNew, "2026-10-16-x.md", fenced).is_ok(), "a `#` line in a code block is not a heading");
        let after = "### A\n```\n# x\n```\n## 2026-10-16\n";
        assert!(parse_entry(Kind::WhatsNew, "2026-10-16-x.md", after).is_err(), "headings after the block count again");
    }

    #[test]
    fn a_changelog_entry_is_one_section() {
        let e = entry(Kind::Changelog, "2026-10-16-x.md", "## Fixes\n\n- **A:** b. (no PR)\n### Detail\n- c\n");
        assert_eq!(e.heading, "Fixes");
        assert_eq!(e.body, "- **A:** b. (no PR)\n### Detail\n- c\n", "`###` subheadings stay in the body");
        for (text, want) in [
            ("### Fixes\n- a\n", "starts with `## <section>`"),
            ("## Fixes\n- a\n## Library\n- b\n", "a second `##` section"),
            ("## Fixes\n# 0.9.1\n", "a `#` heading"),
            ("## Fixes\n", "nothing under the heading"),
        ] {
            let e = parse_entry(Kind::Changelog, "2026-10-16-x.md", text).err().unwrap_or_else(|| panic!("{text:?} should be refused"));
            assert!(e.message.contains(want), "{text:?}: {} should say {want}", e.message);
        }
    }

    const UPSTREAM: &str = "# What's new in {app}\n\n## 2026-10-14\n\n### Up B\n- b\n\n### Up C\n- c\n\n## 2026-10-12\n\n### Up A\n- a\n";

    #[test]
    fn whats_new_joins_by_date_with_ours_first_and_upstream_unchanged() {
        let ours = [
            entry(Kind::WhatsNew, "2026-10-14-zz.md", "### Ours 2\n- two\n"),
            entry(Kind::WhatsNew, "2026-10-14-aa.md", "### Ours 1\n- one\n"),
            entry(Kind::WhatsNew, "2026-10-16-new.md", "### Newest\n- n\n"),
            entry(Kind::WhatsNew, "2026-10-13-mid.md", "### Between\n- m\n"),
        ];
        let joined = join_whats_new(UPSTREAM, &ours);
        let want = "# What's new in {app}\n\n## 2026-10-16\n\n### Newest\n- n\n\n## 2026-10-14\n\n### Ours 1\n- one\n\n### Ours 2\n- two\n\n### Up B\n- b\n\n### Up C\n- c\n\n## 2026-10-13\n\n### Between\n- m\n\n## 2026-10-12\n\n### Up A\n- a\n";
        assert_eq!(joined, want);
        assert_eq!(join_whats_new(UPSTREAM, &[]), UPSTREAM, "upstream's file alone comes out unchanged");
        assert!(join_whats_new("", &ours).starts_with("# What's new in {app}\n\n## 2026-10-16\n"), "a title even without upstream's");
    }

    #[test]
    fn whats_new_merges_a_date_upstream_lists_twice() {
        let up = "# T\n\n## 2026-10-14\n\n### A\n- a\n\n## 2026-10-12\n\n### B\n- b\n\n## 2026-10-14\n\n### C\n- c\n";
        let joined = join_whats_new(up, &[]);
        assert_eq!(joined, "# T\n\n## 2026-10-14\n\n### A\n- a\n\n### C\n- c\n\n## 2026-10-12\n\n### B\n- b\n");
        let fenced = "# T\n\n## 2026-10-14\n\n### A\n```\n## not a date\n```\n";
        assert_eq!(join_whats_new(fenced, &[]), fenced, "a `##` line in upstream's code block starts no section");
        let mixed = "# T\n\n## 2026-10-14\n\n### A\n```\n~~~\n## still code\n```\n";
        assert_eq!(join_whats_new(mixed, &[]), mixed, "a fence closes only on its own mark");
        let undated = "# T\n\n## Earlier releases\n\n- old\n\n## 2026-10-12\n\n### B\n- b\n";
        let ours = [entry(Kind::WhatsNew, "2026-10-16-x.md", "### X\n- x\n")];
        assert_eq!(
            join_whats_new(undated, &ours),
            "# T\n\n## 2026-10-16\n\n### X\n- x\n\n## 2026-10-12\n\n### B\n- b\n\n## Earlier releases\n\n- old\n",
            "a section that isn't a date follows the dates"
        );
        let suffixed = "# T\n\n## 2026-10-14 (0.9.0)\n\n### B\n- b  \n  hard break\n\n## 2026-10-12\n\n### C\n- c\n\n## Earlier releases\n\n- old\n";
        assert_eq!(join_whats_new(suffixed, &[]), suffixed, "a dated heading with a suffix stays among the dates, lines as written");
    }

    #[test]
    fn a_new_release_file_lists_sections_in_the_usual_order() {
        let entries = [
            entry(Kind::Changelog, "2026-10-17-b.md", "## Fixes\n- **Later fix**\n"),
            entry(Kind::Changelog, "2026-10-16-a.md", "## Fixes\n- **Earlier fix**\n"),
            entry(Kind::Changelog, "2026-10-16-z.md", "## Plug-ins\n- **Odd one**\n"),
            entry(Kind::Changelog, "2026-10-18-c.md", "## Library\n- **Grid**\n"),
            entry(Kind::Changelog, "2026-10-18-d.md", "## Highlights\n- **Big**\n"),
        ];
        let out = release_changelog(None, "0.9.1", &entries);
        let want = "# 0.9.1\n\n## Highlights\n\n- **Big**\n\n## Library\n\n- **Grid**\n\n## Fixes\n\n- **Earlier fix**\n- **Later fix**\n\n## Plug-ins\n\n- **Odd one**\n";
        assert_eq!(out, want);
    }

    #[test]
    fn a_started_release_file_keeps_its_text_and_gains_the_entries() {
        let started = "# 0.9.1\n\nIntro written by hand.\n\n## Fixes\n\n- **By hand**\n\n## Notes\n\nKept as is.\n";
        let entries = [
            entry(Kind::Changelog, "2026-10-16-a.md", "## Fixes\n- **Gathered**\n"),
            entry(Kind::Changelog, "2026-10-16-b.md", "## Library\n- **New section**\n"),
        ];
        let out = release_changelog(Some(started), "0.9.1", &entries);
        let want = "# 0.9.1\n\nIntro written by hand.\n\n## Fixes\n\n- **By hand**\n- **Gathered**\n\n## Notes\n\nKept as is.\n\n## Library\n\n- **New section**\n";
        assert_eq!(out, want);
        let paragraph = release_changelog(Some("# 0.9.1\n\n## Fixes\n\nA paragraph.\n"), "0.9.1", &entries[..1]);
        assert_eq!(paragraph, "# 0.9.1\n\n## Fixes\n\nA paragraph.\n\n- **Gathered**\n", "a paragraph and a list stay apart");
    }

    #[test]
    fn reading_a_folder_reports_every_bad_file_and_skips_finder_files() {
        let dir = std::env::temp_dir().join(format!("lcc-notes-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for (name, text) in [
            ("2026-10-16-b.md", "### B\n- b\n"),
            ("2026-10-15-a.md", "### A\n- a\n"),
            ("README.md", "# How entries work\n"),
            ("upstream.sha256", "abc\n"),
            (".DS_Store", "\0\0"),
            ("notes.txt", "x"),
            ("2026-10-16-bad.md", "- no heading\n"),
        ] {
            std::fs::write(dir.join(name), text).unwrap();
        }
        let errors = read_entries(Kind::WhatsNew, &dir).err().unwrap_or_default();
        let files: Vec<String> = errors.iter().map(|e| Path::new(&e.file).file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
        assert_eq!(files, ["2026-10-16-bad.md", "notes.txt", "sub"], "each bad file is named once: {errors:?}");
        for name in ["notes.txt", "2026-10-16-bad.md"] {
            std::fs::remove_file(dir.join(name)).unwrap();
        }
        std::fs::remove_dir(dir.join("sub")).unwrap();
        let entries = read_entries(Kind::WhatsNew, &dir).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["2026-10-15-a.md", "2026-10-16-b.md"], "oldest first; README, the stamp and .DS_Store skipped");
        assert_eq!(read_entries(Kind::WhatsNew, &dir.join("missing")), Ok(Vec::new()), "no folder, no entries");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
