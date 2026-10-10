//! File-name templates shared by batch rename, import renaming and export file naming: `{name}`,
//! `{num}`, `{seq}` / `{seq:3}`, `{date}` / `{date:%Y%m%d}`, `{folder}`, `{camera}`, `{lens}`, `{iso}`,
//! `{rating}`, `{title}`, `{creator}`, `{ext}`. Unknown tokens stay as typed; characters that aren't
//! allowed in file names become `-`.

use std::path::Path;

use dac_catalog::{Photo, PhotoId, Source};
use serde::Serialize;

/// One planned rename.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenamePlan {
    pub id: u64,
    pub from: String,
    pub to: String,
    /// File sources: the old and new paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_path: Option<String>,
}

pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

pub fn sanitize(s: &str) -> String {
    let t: String =
        s.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '-' } else { c }).collect();
    t.trim().trim_matches('.').trim().to_string()
}

/// `%Y%m%d`-style formatting of an ISO local time.
pub fn format_date(iso: &str, fmt: &str) -> String {
    let part = |a: usize, b: usize| iso.get(a..b).unwrap_or("00").to_string();
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('Y') => out.push_str(&part(0, 4)),
            Some('y') => out.push_str(&part(2, 4)),
            Some('m') => out.push_str(&part(5, 7)),
            Some('d') => out.push_str(&part(8, 10)),
            Some('H') => out.push_str(&part(11, 13)),
            Some('M') => out.push_str(&part(14, 16)),
            Some('S') => out.push_str(&part(17, 19)),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

/// One template token, for help texts and tag pickers. [`TOKENS`] is the single list every UI and
/// command description shows; a test checks that [`expand_tokens`] knows each of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TokenHelp {
    /// The token as typed, e.g. `{seq:3}`.
    pub tag: &'static str,
    /// Other spellings with the same meaning.
    pub aliases: &'static [&'static str],
    pub meaning: &'static str,
}

/// Every template token, in the order the help shows them.
pub const TOKENS: &[TokenHelp] = &[
    TokenHelp { tag: "{name}", aliases: &["{filename}"], meaning: "Original file name without its extension" },
    TokenHelp { tag: "{num}", aliases: &[], meaning: "The number at the end of the original name (IMG_0042 → 0042)" },
    TokenHelp { tag: "{seq}", aliases: &["{n}"], meaning: "Sequence number, counting from the start number" },
    TokenHelp { tag: "{seq:3}", aliases: &["{n:3}"], meaning: "Sequence number zero-padded to N digits (1–9)" },
    TokenHelp { tag: "{date}", aliases: &[], meaning: "Capture date as YYYYMMDD" },
    TokenHelp { tag: "{date:%Y%m%d_%H%M%S}", aliases: &[], meaning: "Capture date and time in your own format (directives below)" },
    TokenHelp { tag: "{folder}", aliases: &[], meaning: "Name of the folder the original is in" },
    TokenHelp { tag: "{camera}", aliases: &[], meaning: "Camera make and model" },
    TokenHelp { tag: "{lens}", aliases: &[], meaning: "Lens" },
    TokenHelp { tag: "{iso}", aliases: &[], meaning: "ISO speed" },
    TokenHelp { tag: "{rating}", aliases: &[], meaning: "Star rating (0–5)" },
    TokenHelp { tag: "{title}", aliases: &[], meaning: "Title metadata" },
    TokenHelp { tag: "{creator}", aliases: &[], meaning: "Creator metadata" },
    TokenHelp { tag: "{ext}", aliases: &[], meaning: "Original extension without the dot" },
];

/// The `%` directives of `{date:…}`.
pub const DATE_DIRECTIVES: &[(&str, &str)] = &[
    ("%Y", "year, 4 digits"),
    ("%y", "year, 2 digits"),
    ("%m", "month 01–12"),
    ("%d", "day 01–31"),
    ("%H", "hour 00–23"),
    ("%M", "minute"),
    ("%S", "second"),
    ("%%", "a literal %"),
];

/// How templates behave, one sentence each (shown under the token list).
pub const TEMPLATE_NOTES: &[&str] = &[
    "The original extension is always added; {ext} only puts it inside the name as well.",
    "A blank template keeps the original names.",
    "{date} is the capture time; a photo without one uses the time it was imported.",
    "Missing metadata ({camera}, {title}…) leaves an empty gap; a name that comes out empty keeps the original name.",
    "Unknown tags stay as typed — check the preview for a {typo}.",
    "Characters not allowed in file names (/ \\ : * ? \" < > |) become -; an existing name gets -1, -2….",
];

/// The tokens as one line (`{name} {num} {seq} …`), for compact hints.
pub fn token_summary() -> String {
    TOKENS.iter().map(|t| t.tag).collect::<Vec<_>>().join(" ")
}

/// A fixed photo the help's examples are computed from (`IMG_0042.CR3`, 14 Jan 2026 05:58:48).
pub fn sample_photo() -> Photo {
    let mut p =
        Photo::new(PhotoId(0), Source::File { path: "/Card/DCIM/IMG_0042.CR3".into() }, "IMG_0042.CR3", "CR3", 6000, 4000, "2026-01-20T10:00:00");
    p.captured = Some("2026-01-14T05:58:48".into());
    p.rating = 4;
    p.meta.camera = "Canon EOS R5".into();
    p.meta.lens = "RF24-70mm F2.8".into();
    p.meta.iso = Some(400);
    p.meta.title = "Harbour".into();
    p.meta.creator = "Ann Lee".into();
    p
}

/// The token as expanded for [`sample_photo`] (sequence number 1), for the help's example column.
pub fn token_example(tag: &str) -> String {
    expand_tokens(tag, &sample_photo(), 1, 1)
}

/// Why a folder template (`{date:%Y}/{date:%Y%m%d}`) can't be used, if it can't: it must be
/// relative (no leading `/` or `\`, drive letter or `~`) and have no `.` / `..` levels.
pub fn folder_template_error(template: &str) -> Option<String> {
    let t = template.trim();
    if t.is_empty() {
        return Some("the folder template is empty".into());
    }
    let b = t.as_bytes();
    if t.starts_with(['/', '\\', '~']) || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':') {
        return Some("the folder template must be relative to the destination (no leading /, \\, ~ or drive)".into());
    }
    if t.split(['/', '\\']).any(|s| matches!(s.trim(), "." | "..")) {
        return Some("the folder template may not contain . or .. folders".into());
    }
    None
}

/// The folders (relative to the destination) a folder template gives `p`: the template's own `/`
/// (or `\`) separate levels; each level's tokens are expanded and the result made a safe folder
/// name — a value can never add a level or climb out (`/`, `\`, `:`… become `-`, leading and
/// trailing dots are dropped). A level that comes out empty (missing metadata) is `unknown`; empty
/// levels in the template (`a//b`) are skipped. `{date}` is the capture time, else [`Photo::date`]'s
/// fallback (the import time). Check [`folder_template_error`] first; levels it rejects are skipped.
pub fn expand_folder(template: &str, p: &Photo, seq: usize) -> Vec<String> {
    template
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|s| !s.is_empty() && !matches!(*s, "." | ".."))
        .map(|s| {
            let v = sanitize(&expand_tokens(s, p, seq, 1));
            if v.is_empty() { "unknown".to_string() } else { v }
        })
        .collect()
}

/// The `{…}` tags in `template` that aren't tokens (they stay as typed), for a warning.
pub fn unknown_tokens(template: &str) -> Vec<String> {
    let p = sample_photo();
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        let tag = &rest[i..i + j + 1];
        if expand_tokens(tag, &p, 1, 1) == tag && !out.iter().any(|t| t == tag) {
            out.push(tag.to_string());
        }
        rest = &rest[i + j + 1..];
    }
    out
}

/// Every token with its meaning and example, the date directives and the notes, as JSON (the
/// `photo.renameTokens` command).
pub fn token_help_json() -> serde_json::Value {
    let tokens: Vec<serde_json::Value> = TOKENS
        .iter()
        .map(|t| serde_json::json!({"tag": t.tag, "aliases": t.aliases, "meaning": t.meaning, "example": token_example(t.tag)}))
        .collect();
    let directives: Vec<serde_json::Value> = DATE_DIRECTIVES.iter().map(|(d, m)| serde_json::json!({"directive": d, "meaning": m})).collect();
    serde_json::json!({
        "tokens": tokens,
        "dateDirectives": directives,
        "notes": TEMPLATE_NOTES,
        "sample": "IMG_0042.CR3, captured 2026-01-14 05:58:48",
    })
}

/// Expand `template` for `p` (sequence number `seq`) into a file name with `p`'s extension.
pub fn expand(template: &str, p: &Photo, seq: usize) -> String {
    let (stem, ext) = split_ext(&p.file_name);
    let out = expand_tokens(template, p, seq, 1);
    let mut stem_out = sanitize(&out);
    if stem_out.is_empty() {
        stem_out = sanitize(stem);
    }
    if stem_out.is_empty() {
        stem_out = "photo".into();
    }
    if ext.is_empty() { stem_out } else { format!("{stem_out}.{ext}") }
}

/// The template's tokens replaced for `p` (no extension added, nothing sanitized). A bare `{seq}`
/// is zero-padded to `seq_width` digits.
pub fn expand_tokens(template: &str, p: &Photo, seq: usize, seq_width: usize) -> String {
    let (stem, ext) = split_ext(&p.file_name);
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('}') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let tok = &rest[i + 1..i + j];
        let (name, arg) = tok.split_once(':').map(|(a, b)| (a, Some(b))).unwrap_or((tok, None));
        let v = match name.trim().to_ascii_lowercase().as_str() {
            "name" | "filename" => stem.to_string(),
            "num" => {
                let digits = stem.chars().rev().take_while(char::is_ascii_digit).count();
                stem[stem.len() - digits..].to_string()
            }
            "seq" | "n" => {
                let w: usize = arg.and_then(|a| a.parse().ok()).unwrap_or(seq_width).min(9);
                format!("{seq:0w$}")
            }
            "date" => format_date(p.date(), arg.unwrap_or("%Y%m%d")),
            "folder" => match &p.source {
                Source::File { path } => {
                    Path::new(path).parent().and_then(Path::file_name).map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
                }
                Source::Demo { .. } => String::new(),
            },
            "camera" => p.meta.camera.clone(),
            "lens" => p.meta.lens.clone(),
            "iso" => p.meta.iso.map(|v| v.to_string()).unwrap_or_default(),
            "rating" => p.rating.to_string(),
            "title" => p.meta.title.clone(),
            "creator" => p.meta.creator.clone(),
            "ext" => ext.to_string(),
            _ => format!("{{{tok}}}"),
        };
        out.push_str(&v);
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}
