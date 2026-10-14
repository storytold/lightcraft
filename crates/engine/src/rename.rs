//! Batch rename: a file-name template applied to the selected photos, renaming the files on
//! disk (with their XMP sidecars) safely.
//!
//! Template tokens (shared with import renaming and export file naming): `{name}` (current name
//! without extension), `{num}` (the number at the end of the name: `IMG_0042` → `0042`), `{seq}` /
//! `{seq:3}` (sequence number, zero-padded to 3 digits), `{date}` / `{date:%Y%m%d}` (capture date;
//! `%Y %y %m %d %H %M %S`), `{folder}` (the original's folder name), `{camera}`, `{lens}`, `{iso}`,
//! `{rating}`, `{title}`, `{creator}`, `{ext}` (extension, without the dot). Unknown tokens stay as
//! typed. Characters that aren't allowed in file names become `-`. The original extension is always
//! kept.
//!
//! Safety:
//! - a target that exists on disk, or that another photo in the batch gets, receives a `-1`, `-2`…
//!   suffix — no file is ever overwritten. Whether an existing target is the photo itself (a
//!   change of letter case on a case-insensitive volume) is decided by file identity, not by
//!   comparing names, so on a case-sensitive volume `img_1.jpg` next to `IMG_1.JPG` is a collision
//!   (issue #95); moves never replace a file even if one appears after the check (hard link +
//!   unlink where the volume supports it);
//! - the moves happen before the catalog changes; if one fails, the ones already done are moved
//!   back and nothing is committed;
//! - the catalog op ([`Op::SetFile`]) is undoable: undo/redo move the files back and forth (see
//!   [`crate::Session::undo_step`]);
//! - virtual copies follow their master's file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde::Serialize;

use crate::sidecar::SidecarNaming;
use crate::{EngineError, Result, Session};

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

fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

fn sanitize(s: &str) -> String {
    let t: String =
        s.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '-' } else { c }).collect();
    t.trim().trim_matches('.').trim().to_string()
}

/// `%Y%m%d`-style formatting of an ISO local time.
fn format_date(iso: &str, fmt: &str) -> String {
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

/// Every sidecar that may belong to `path` (both naming conventions, `.xmp` and `.XMP`).
fn sidecars(path: &str) -> Vec<(PathBuf, SidecarNaming, bool)> {
    let mut v = Vec::new();
    for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
        let p = crate::sidecar::sidecar_path(path, naming);
        v.push((p.with_extension("XMP"), naming, true));
        v.push((p, naming, false));
    }
    v
}

/// The file-system primitives renaming uses. [`RealFs`] is the disk; tests substitute a model of
/// a case-insensitive or case-sensitive volume (and inject failures).
pub(crate) trait MoveFs {
    /// Something (file, folder, link) answers to `p`.
    fn exists(&self, p: &Path) -> bool;
    fn is_file(&self, p: &Path) -> bool;
    /// `a` and `b` name the same file (e.g. `IMG.JPG` and `img.jpg` on a case-insensitive
    /// volume). Decided by file identity, never by comparing names: on a case-sensitive volume
    /// they are two different photos. When in doubt: `false`.
    fn same_file(&self, a: &Path, b: &Path) -> bool;
    /// Rename `a` to `b`, failing (never replacing) when `b` exists.
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()>;
    /// Copy `a` to a new file `b` (an independent copy, never a link), failing when `b` exists.
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()>;
    fn remove_file(&self, p: &Path) -> std::io::Result<()>;
    /// Another photo file beside the stem-named sidecar `sidecar` that shares its stem (e.g.
    /// `IMG_1.JPG` for `IMG_1.xmp`), which still uses that sidecar.
    fn stem_sibling(&self, sidecar: &Path) -> Option<String>;
    /// The folder lists an entry spelled exactly like `p` (letter case included). On a
    /// case-insensitive volume `a.XMP` "exists" when only `a.xmp` is there; this tells the two
    /// apart. When the folder can't be listed: `true`, so nothing is skipped.
    fn listed(&self, p: &Path) -> bool {
        let (Some(dir), Some(name)) = (p.parent(), p.file_name()) else { return true };
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        match std::fs::read_dir(dir) {
            Ok(entries) => entries.flatten().any(|e| e.file_name() == name),
            Err(_) => true,
        }
    }
    /// Content observed in a packet, independent of its pathname or inode. Model volumes
    /// override this; disk implementations stream a bounded buffer without retaining the XML.
    fn packet_version(&self, path: &Path) -> std::io::Result<PacketVersion> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hash = lightcraft_preview::Hasher128::new();
        let mut len = 0u64;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let n = match file.read(&mut buffer) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if n == 0 {
                return Ok(PacketVersion { len, hash: hash.finish() });
            }
            let bytes = buffer.get(..n).ok_or_else(|| std::io::Error::other("invalid sidecar read length"))?;
            len = len.checked_add(n as u64).ok_or_else(|| std::io::Error::other("sidecar is too large"))?;
            hash.update(bytes);
        }
    }
}

/// The real file system.
pub(crate) struct RealFs;

impl MoveFs for RealFs {
    fn exists(&self, p: &Path) -> bool {
        std::fs::symlink_metadata(p).is_ok()
    }

    fn is_file(&self, p: &Path) -> bool {
        p.is_file()
    }

    fn same_file(&self, a: &Path, b: &Path) -> bool {
        if a == b {
            return true;
        }
        let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ma.dev() == mb.dev() && ma.ino() == mb.ino()
        }
        #[cfg(not(unix))]
        {
            // no stable file id in std here: `a` and `b` are one file under two spellings when
            // they look alike and the folder doesn't list both (on a case-sensitive folder
            // holding both `IMG.JPG` and `img.jpg`, both entries are listed). Either may be the
            // listed one: the answer must not depend on the argument order.
            let (Some(dir), Some(name_a), Some(name_b)) = (b.parent(), a.file_name(), b.file_name()) else { return false };
            let respelled = name_a.to_string_lossy().to_lowercase() == name_b.to_string_lossy().to_lowercase();
            if !respelled || a.parent() != Some(dir) || ma.len() != mb.len() || ma.modified().ok() != mb.modified().ok() {
                return false;
            }
            let Ok(entries) = std::fs::read_dir(dir) else { return false };
            let (mut listed_a, mut listed_b) = (false, false);
            for e in entries.flatten() {
                let name = e.file_name();
                listed_a |= name == name_a;
                listed_b |= name == name_b;
            }
            !(listed_a && listed_b)
        }
    }

    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if self.exists(b) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", b.display())));
        }
        // A hard link fails when `b` exists — even if it appeared since the check above — so
        // nothing is ever replaced. Volumes without hard links (FAT, some network shares) fall back
        // to a plain rename after the check.
        match std::fs::hard_link(a, b) {
            Ok(()) => std::fs::remove_file(a).inspect_err(|_| {
                // both names are the same file: dropping the new one loses nothing
                let _ = std::fs::remove_file(b);
            }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
            Err(_) => std::fs::rename(a, b)
                .or_else(|e| if e.kind() == std::io::ErrorKind::CrossesDevices { move_across_volumes(self, a, b) } else { Err(e) }),
        }
    }

    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let bytes = std::fs::read(a)?;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(b)?;
        f.write_all(&bytes).and_then(|_| f.sync_all()).inspect_err(|_| {
            let _ = std::fs::remove_file(b);
        })
    }

    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        std::fs::remove_file(p)
    }

    fn stem_sibling(&self, sidecar: &Path) -> Option<String> {
        crate::import_move::sibling_photo(sidecar)
    }
}

/// Move `a` to `b` on another volume: copy (never replacing), compare the copy with the original
/// byte for byte, and only then remove the original. A copy that doesn't match is removed.
fn move_across_volumes(fs: &RealFs, a: &Path, b: &Path) -> std::io::Result<()> {
    fs.copy_no_replace(a, b)?;
    if !same_contents(a, b).unwrap_or(false) {
        let _ = std::fs::remove_file(b);
        return Err(std::io::Error::other(format!("the copy at {} did not match the original; the original was kept", b.display())));
    }
    std::fs::remove_file(a).inspect_err(|_| {
        // the original stays: drop the copy so the file isn't in two places
        let _ = std::fs::remove_file(b);
    })
}

/// Whether files `a` and `b` hold the same bytes: lengths first, then 1 MiB chunks at a time, so
/// a large raw or video is never read into memory whole.
fn same_contents(a: &Path, b: &Path) -> std::io::Result<bool> {
    const CHUNK: usize = 1 << 20;
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba.get(..n) != bb.get(..m) {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Fill `buf` from `r` as far as the file goes; the number of bytes read (less than the buffer
/// only at the end of the file).
fn read_full(r: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|r| !r.is_empty()) {
        match r.read(rest) {
            Ok(0) => break,
            Ok(k) => filled = filled.saturating_add(k),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// `from` → `to` differs only in letter case and `to` is that very file (a case-insensitive
/// volume): the rename must go through a temporary name.
fn is_respelling(fs: &dyn MoveFs, from: &Path, to: &Path) -> bool {
    from != to && folded_path(&from.to_string_lossy()) == folded_path(&to.to_string_lossy()) && fs.exists(to) && fs.same_file(from, to)
}

/// Windows accepts both separators; spelling differences must not hide file identity.
fn folded_path(path: &str) -> String {
    if cfg!(windows) { path.replace('\\', "/").to_lowercase() } else { path.to_lowercase() }
}

/// Rename `a` to `b` without ever replacing a file; a change of letter case only goes through a
/// unique temporary name so case-insensitive volumes see a real change.
fn move_one(fs: &dyn MoveFs, a: &Path, b: &Path) -> std::io::Result<()> {
    if !is_respelling(fs, a, b) {
        return fs.rename_no_replace(a, b);
    }
    let tmp = (0..1000u32)
        .map(|k| a.with_file_name(format!(".lc-rename-{}-{k}", std::process::id())))
        .find(|t| !fs.exists(t))
        .ok_or_else(|| std::io::Error::other("no free temporary name"))?;
    fs.rename_no_replace(a, &tmp)?;
    fs.rename_no_replace(&tmp, b).map_err(|e| match fs.rename_no_replace(&tmp, a) {
        Ok(()) => e,
        Err(e2) => std::io::Error::other(format!("{e}; it could not be given its old name back either ({e2}): it is now {}", tmp.display())),
    })
}

/// An unfinished sidecar operation. Keep its exact paths and any recorded copy requirement: the photo may
/// already have followed its new path, so a retry cannot infer this work from the catalog.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SidecarMove {
    pub from: PathBuf,
    pub to: PathBuf,
    copied: bool,
    /// Observed packets in the partial move, including the separated source. Absent planned
    /// destinations are never owned; a completed repair advances these observations.
    companions: Vec<RetainedPacket>,
}

/// A content version, not an assertion that a pathname still names the same packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PacketVersion {
    len: u64,
    hash: lightcraft_preview::Hash128,
}

impl PacketVersion {
    fn of(bytes: &[u8]) -> Self {
        Self { len: bytes.len() as u64, hash: lightcraft_preview::hash_bytes(bytes) }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct RetainedPacket {
    path: PathBuf,
    // An unreadable observation remains untrusted even if a later read succeeds.
    version: Option<PacketVersion>,
}

impl RetainedPacket {
    fn validate(&self, fs: &dyn MoveFs) -> std::result::Result<(), String> {
        if self.version.is_none() {
            return Err(format!("can't verify retained XMP {}: its content was unreadable when the partial move was recorded", self.path.display()));
        }
        let current = fs.packet_version(&self.path).map_err(|e| format!("can't verify retained XMP {}: {e}", self.path.display()))?;
        if self.version != Some(current) {
            return Err(format!(
                "retained XMP {} changed since the partial move; packet ownership must be resolved before retrying",
                self.path.display()
            ));
        }
        Ok(())
    }
}

/// Advance only packets whose operation actually succeeded. A copy retains its original;
/// a move retires vanished source aliases and adds the observed destination.
fn advance_packets(packets: &mut Vec<RetainedPacket>, aliases: &[PathBuf], to: &Path, copied: bool, fs: &dyn MoveFs) {
    let Some(mut packet) = packets.iter().find(|p| aliases.contains(&p.path)).cloned() else { return };
    if !copied {
        packets.retain(|p| !aliases.contains(&p.path) || fs.is_file(&p.path));
    }
    packet.path = to.to_path_buf();
    if !packets.iter().any(|p| p.path == packet.path) {
        packets.push(packet);
    }
}

fn packet_aliases(packets: &[RetainedPacket], fs: &dyn MoveFs, from: &Path) -> Vec<PathBuf> {
    packets.iter().filter(|p| p.path == from || fs.same_file(&p.path, from)).map(|p| p.path.clone()).collect()
}

impl SidecarMove {
    fn validate_packets(&self, fs: &dyn MoveFs) -> std::result::Result<(), String> {
        for packet in &self.companions {
            packet.validate(fs)?;
        }
        Ok(())
    }

    /// Validate a queued retry and return the copy decision for this physical operation.
    fn validate_repair(&self, fs: &dyn MoveFs) -> std::result::Result<bool, String> {
        self.validate_packets(fs)?;
        let respelling = is_respelling(fs, &self.from, &self.to);
        // Batch rollback may have restored another photo using this source packet.
        let copied = self.copied || (!respelling && fs.stem_sibling(&self.from).is_some());
        if fs.exists(&self.to) && (copied || !respelling) {
            return Err(format!("{} already exists while repairing {} → {}", self.to.display(), self.from.display(), self.to.display()));
        }
        Ok(copied)
    }

    fn needed(&self, fs: &dyn MoveFs) -> bool {
        fs.is_file(&self.from) && !(fs.exists(&self.to) && (self.copied || !is_respelling(fs, &self.from, &self.to)))
    }

    fn touches(&self, fs: &dyn MoveFs, path: &Path) -> bool {
        let same_path = |endpoint: &Path| {
            endpoint == path
                || fs.same_file(endpoint, path)
                || (endpoint.file_name() == path.file_name()
                    && endpoint.parent().zip(path.parent()).is_some_and(|(a, b)| a == b || fs.same_file(a, b)))
        };
        same_path(&self.from) || same_path(&self.to)
    }

    fn apply(&self, fs: &dyn MoveFs) -> std::io::Result<()> {
        if self.copied { fs.copy_no_replace(&self.from, &self.to) } else { move_one(fs, &self.from, &self.to) }
    }
}

/// Complete known unfinished sidecars before moving any more photos. Completed operations
/// leave the queue; a failure retains that operation and the rest, without replacing a file.
pub(crate) fn retry_sidecars(fs: &dyn MoveFs, pending: &mut Vec<SidecarMove>) -> std::result::Result<(), String> {
    validate_sidecars(fs, pending)?;
    while !pending.is_empty() {
        apply_repair(fs, pending, 0)?;
    }
    Ok(())
}

/// Preflight a detached history entry before global queues are allowed to retire. The
/// caller can validate without mutating it; retry revalidates before each physical operation.
pub(crate) fn validate_sidecars(fs: &dyn MoveFs, pending: &[SidecarMove]) -> std::result::Result<(), String> {
    for repair in pending {
        repair.validate_repair(fs)?;
    }
    Ok(())
}

/// Finish one operation and transfer its observed content to the remaining repairs. Do not
/// infer ownership by reading an occupied destination or by retaining a planned pathname.
fn apply_repair(fs: &dyn MoveFs, pending: &mut Vec<SidecarMove>, i: usize) -> std::result::Result<(), String> {
    let Some(mut repair) = pending.get(i).cloned() else { return Ok(()) };
    repair.copied = repair.validate_repair(fs)?;
    let aliases = packet_aliases(&repair.companions, fs, &repair.from);
    repair.apply(fs).map_err(|e| format!("rename {} → {}: {e}", repair.from.display(), repair.to.display()))?;
    for remaining in pending.iter_mut() {
        advance_packets(&mut remaining.companions, &aliases, &repair.to, repair.copied, fs);
    }
    pending.remove(i);
    Ok(())
}

/// Repair only operations that affect a sidecar being saved. Unrelated unavailable originals
/// must not block healthy metadata saves. Completed repairs retire; failures stay queued.
pub(crate) fn retry_sidecars_for_paths(fs: &dyn MoveFs, pending: &mut Vec<SidecarMove>, paths: &[PathBuf]) -> std::result::Result<(), String> {
    for repair in pending.iter().filter(|m| paths.iter().any(|path| m.touches(fs, path))) {
        repair.validate_repair(fs)?;
    }
    let mut i = 0;
    while let Some(m) = pending.get(i) {
        if paths.iter().any(|path| m.touches(fs, path)) {
            // Path/alias equality only locates an observation; validation above proves its
            // content. A newly occupied, previously absent peer destination is a collision.
            if let Some(path) = paths.iter().find(|path| {
                fs.exists(path)
                    && !fs.same_file(path, &m.from)
                    && !fs.same_file(path, &m.to)
                    && !m.companions.iter().any(|known| fs.same_file(path, &known.path))
            }) {
                return Err(format!("{} already exists while repairing {} → {}", path.display(), m.from.display(), m.to.display()));
            }
            apply_repair(fs, pending, i)?;
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// Every record came from an actual packet observed before this move. Successful physical
/// operations have already advanced its path; missing planned destinations are not recorded.
fn note_companions(pending: &mut [SidecarMove], packets: Vec<RetainedPacket>) {
    for repair in pending {
        repair.companions.clone_from(&packets);
    }
}

/// Why [`move_file`] failed, and what it left behind.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveError {
    /// What went wrong, including every step that could not be undone and where that file is now.
    pub message: String,
    /// The file itself ended up at the new path: it moved, a sidecar didn't, and moving the file
    /// back failed too. Its sidecars that did move stay with it.
    pub moved: bool,
    pub(crate) pending_sidecars: Vec<SidecarMove>,
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Move `from` to `to` (never over an existing file) together with its sidecars. On error the
/// moves are undone; whatever can't be undone is named in the error (see [`MoveError`]).
pub fn move_file(from: &str, to: &str) -> std::result::Result<(), MoveError> {
    move_file_with(&RealFs, from, to)
}

/// [`move_file`] on the file system `fs`.
pub(crate) fn move_file_with(fs: &dyn MoveFs, from: &str, to: &str) -> std::result::Result<(), MoveError> {
    if Path::new(from) == Path::new(to) {
        return Ok(());
    }
    let fail = |message: String| MoveError { message, moved: false, pending_sidecars: Vec::new() };
    let (f, t) = (Path::new(from), Path::new(to));
    // `to` may be this very file spelled differently (case-insensitive volume); any other file
    // there — e.g. `img.jpg` next to `IMG.JPG` on a case-sensitive volume — is never replaced
    if fs.exists(t) && !is_respelling(fs, f, t) {
        return Err(fail(format!("{to} already exists")));
    }
    // An upper-case `.XMP` spelling that only answers through a case-insensitive volume is the
    // lower-case sidecar: track and move it under its own name, so `a.xmp` doesn't become `b.XMP`.
    let present = |(sc, _, upper): &(PathBuf, SidecarNaming, bool)| fs.is_file(sc) && (!*upper || fs.listed(sc));
    let mut packets: Vec<RetainedPacket> =
        sidecars(from).into_iter().filter(present).map(|(path, _, _)| RetainedPacket { version: fs.packet_version(&path).ok(), path }).collect();
    move_one(fs, f, t).map_err(|e| fail(format!("rename {from}: {e}")))?;
    let mut done: Vec<SidecarMove> = Vec::new();
    let mut remaining = sidecars(from).into_iter().filter(present).map(|(sc, naming, upper)| {
        let mut dst = crate::sidecar::sidecar_path(to, naming);
        if upper {
            dst = dst.with_extension("XMP");
        }
        // A stem sidecar another photo still uses is copied, not taken away (issue #92).
        let copied = naming == SidecarNaming::Stem && fs.stem_sibling(&sc).is_some();
        SidecarMove { from: sc, to: dst, copied, companions: Vec::new() }
    });
    while let Some(m) = remaining.next() {
        if !m.needed(fs) {
            continue; // never overwrite: the old sidecar stays where it was
        }
        let aliases = packet_aliases(&packets, fs, &m.from);
        if let Err(e) = m.apply(fs) {
            let mut message = format!("rename {}: {e}", m.from.display());
            // The file first: if it can't go back, sidecars that moved stay with it.
            if let Err(e) = move_one(fs, t, f) {
                message.push_str(&format!("; {from} could not be moved back ({e}): it is now {to}"));
                if !done.is_empty() {
                    let names: Vec<String> = done.iter().map(|m| m.to.display().to_string()).collect();
                    message.push_str(&format!(" with its sidecar {}", names.join(", ")));
                }
                let mut pending_sidecars = vec![m];
                for m in remaining.filter(|m| m.needed(fs)) {
                    // The two case spellings can name one file. Keep one operation for
                    // those aliases, while retaining distinct sidecars on sensitive volumes.
                    let alias = pending_sidecars.iter().any(|p| {
                        folded_path(&p.from.to_string_lossy()) == folded_path(&m.from.to_string_lossy())
                            && folded_path(&p.to.to_string_lossy()) == folded_path(&m.to.to_string_lossy())
                            && fs.same_file(&p.from, &m.from)
                    });
                    if !alias {
                        pending_sidecars.push(m);
                    }
                }
                note_companions(&mut pending_sidecars, packets);
                return Err(MoveError { message, moved: true, pending_sidecars });
            }
            let mut pending_sidecars = Vec::new();
            for m in done.iter().rev() {
                if m.copied {
                    // The original is still there: a leftover copy loses nothing.
                    if fs.remove_file(&m.to).is_ok() {
                        packets.retain(|p| p.path != m.to);
                    }
                } else {
                    let aliases = packet_aliases(&packets, fs, &m.to);
                    if let Err(e) = move_one(fs, &m.to, &m.from) {
                        message.push_str(&format!("; sidecar {} could not be moved back ({e}): it is now {}", m.from.display(), m.to.display()));
                        pending_sidecars.push(SidecarMove { from: m.to.clone(), to: m.from.clone(), copied: false, companions: Vec::new() });
                    } else {
                        advance_packets(&mut packets, &aliases, &m.from, false, fs);
                    }
                }
            }
            note_companions(&mut pending_sidecars, packets);
            return Err(MoveError { message, moved: false, pending_sidecars });
        }
        advance_packets(&mut packets, &aliases, &m.to, m.copied, fs);
        done.push(m);
    }
    Ok(())
}

/// What a failed batch of moves ([`move_all`]) left behind.
#[derive(Debug)]
pub(crate) struct BatchMoveError {
    /// The first failure and every move that could not be undone.
    pub message: String,
    /// Moves that stayed done, (from, to): those files are at `to` now.
    pub stuck: Vec<(String, String)>,
    /// Sidecars still separated from the path where their photo ended up, in either direction.
    pub pending_sidecars: Vec<SidecarMove>,
}

/// Move the files `moves` names, all or nothing: after a failure the earlier moves are undone.
/// Moves that can't be undone (e.g. the network share went away) are reported, not ignored.
pub(crate) fn move_all(fs: &dyn MoveFs, moves: &[(String, String)]) -> std::result::Result<(), BatchMoveError> {
    let mut done: Vec<&(String, String)> = Vec::new();
    for m in moves {
        let Err(e) = move_file_with(fs, &m.0, &m.1) else {
            done.push(m);
            continue;
        };
        let mut message = e.message;
        let mut pending_sidecars = e.pending_sidecars;
        let mut stuck = Vec::new();
        if e.moved {
            stuck.push(m.clone());
        }
        for d in done.iter().rev() {
            if let Err(mut e) = move_file_with(fs, &d.1, &d.0) {
                message.push_str(&format!("; {} could not be moved back to {}: {}", d.1, d.0, e.message));
                if !e.moved {
                    stuck.push((*d).clone());
                }
                pending_sidecars.append(&mut e.pending_sidecars);
            }
        }
        return Err(BatchMoveError { message, stuck, pending_sidecars });
    }
    Ok(())
}

/// Final `SetFile` per photo (nested batches included), filtered by destination only afterward.
fn set_file_ops_to(op: &Op, to: &HashSet<&str>, out: &mut Vec<Op>) {
    let mut pending = vec![op];
    let mut order = Vec::new();
    let mut final_ops = HashMap::new();
    while let Some(op) = pending.pop() {
        match op {
            Op::Batch { ops } => pending.extend(ops.iter().rev()),
            Op::SetFile { id, .. } if final_ops.insert(*id, op).is_none() => order.push(*id),
            _ => {}
        }
    }
    out.extend(order.into_iter().filter_map(|id| {
        let op = final_ops.get(&id).copied()?;
        matches!(op, Op::SetFile { source: Source::File { path }, .. } if to.contains(path.as_str())).then(|| op.clone())
    }));
}

/// The photos among `ids` a rename handles, in order: photos sharing one file (virtual copies)
/// rename once. No file-system calls.
pub fn rename_photos(cat: &lightcraft_catalog::Catalog, ids: &[PhotoId]) -> Vec<std::sync::Arc<Photo>> {
    let mut seen_paths: HashSet<String> = HashSet::new();
    ids.iter()
        .filter_map(|id| cat.photo(*id))
        .filter(|p| match &p.source {
            Source::File { path } => seen_paths.insert(path.clone()),
            _ => true,
        })
        .cloned()
        .collect()
}

/// [`Session::plan_rename`] for [`rename_photos`], with the "is this name taken on disk?" check
/// supplied: the Rename dialog plans its preview on a worker thread (a check can block on a slow
/// drive). Whether a case-only change names this very file is decided on the disk.
pub fn plan_rename_photos(photos: &[std::sync::Arc<Photo>], template: &str, start: usize, exists: &dyn Fn(&str) -> bool) -> Vec<RenamePlan> {
    plan_rename_core(photos, template, start, &|p| exists(&p.to_string_lossy()), &|a, b| RealFs.same_file(a, b))
}

/// Plan renaming `photos`: `exists` says whether something answers to a path, `same_file` whether
/// two spellings name one file (see [`MoveFs`]).
fn plan_rename_core(
    photos: &[std::sync::Arc<Photo>],
    template: &str,
    start: usize,
    exists: &dyn Fn(&Path) -> bool,
    same_file: &dyn Fn(&Path, &Path) -> bool,
) -> Vec<RenamePlan> {
    let mut taken: HashSet<String> = HashSet::new(); // lower-case target paths/names claimed in this batch
    let mut out = Vec::new();
    for (seq, p) in (start..).zip(photos) {
        let want = expand(template, p, seq);
        let (stem, ext) = split_ext(&want);
        let (stem, ext) = (stem.to_string(), ext.to_string());
        let candidate = |k: usize| {
            if k == 0 {
                want.clone()
            } else if ext.is_empty() {
                format!("{stem}-{k}")
            } else {
                format!("{stem}-{k}.{ext}")
            }
        };
        let (to, to_path) = match &p.source {
            Source::File { path } => {
                let dir = Path::new(path).parent().map(Path::to_path_buf).unwrap_or_default();
                let mut k = 0;
                loop {
                    let name = candidate(k);
                    let tp = dir.join(&name).to_string_lossy().to_string();
                    let key = folded_path(&tp);
                    // this very file, maybe spelled differently (case-insensitive volume)? By
                    // identity: on a case-sensitive volume `img_1.jpg` may be another photo
                    let (from, to) = (Path::new(path), Path::new(&tp));
                    let same = from == to || (key == folded_path(path) && exists(to) && same_file(from, to));
                    // free: not claimed in this batch and not on disk (unless it is this very file).
                    // A file this batch moves away still counts as taken: simple and safe.
                    if !taken.contains(&key) && (same || !exists(to)) {
                        taken.insert(key);
                        break (name, Some(tp));
                    }
                    k += 1;
                }
            }
            Source::Demo { .. } => {
                let mut k = 0;
                loop {
                    let name = candidate(k);
                    if taken.insert(format!("demo:{}", name.to_lowercase())) {
                        break (name, None);
                    }
                    k += 1;
                }
            }
        };
        let from_path = match &p.source {
            Source::File { path } => Some(path.clone()),
            _ => None,
        };
        out.push(RenamePlan { id: p.id.0, from: p.file_name.clone(), to, from_path, to_path });
    }
    out
}

impl Session {
    /// Plan renaming `ids` with `template` (sequence numbers from `start`), resolving collisions.
    pub fn plan_rename(&self, ids: &[PhotoId], template: &str, start: usize) -> Vec<RenamePlan> {
        self.plan_rename_with(&RealFs, ids, template, start)
    }

    /// [`Session::plan_rename`] on the file system `fs`.
    pub(crate) fn plan_rename_with(&self, fs: &dyn MoveFs, ids: &[PhotoId], template: &str, start: usize) -> Vec<RenamePlan> {
        plan_rename_core(&rename_photos(&self.catalog, ids), template, start, &|p| fs.exists(p), &|a, b| fs.same_file(a, b))
    }

    /// Carry out `plans`: move the files (rolled back on failure), then commit one undoable op
    /// that updates the photos (and their virtual copies).
    ///
    /// When a failure can't be fully rolled back, the files that kept their new names are
    /// committed as a smaller undoable rename, so the library points at them (nothing goes
    /// missing), and the error lists them (old → new).
    pub fn apply_rename(&mut self, plans: &[RenamePlan]) -> Result<usize> {
        self.apply_rename_with(&RealFs, plans)
    }

    /// Complete retained repairs before a file or folder change can stale their exact paths.
    pub(crate) fn repair_sidecars_with(&mut self, fs: &dyn MoveFs) -> Result<()> {
        for repair in self.pending_sidecar_repairs.iter().chain(self.undo.iter().chain(&self.redo).flat_map(|e| &e.retry_sidecars)) {
            repair.validate_repair(fs).map_err(EngineError::Other)?;
        }
        retry_sidecars(fs, &mut self.pending_sidecar_repairs).map_err(EngineError::Other)?;
        for entry in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            retry_sidecars(fs, &mut entry.retry_sidecars).map_err(EngineError::Other)?;
        }
        Ok(())
    }

    pub(crate) fn repair_sidecars_for_paths(&mut self, paths: &[PathBuf], fs: &dyn MoveFs) -> Result<()> {
        self.validate_sidecar_packets_for_paths(paths, fs)?;
        retry_sidecars_for_paths(fs, &mut self.pending_sidecar_repairs, paths).map_err(EngineError::Other)?;
        for entry in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            retry_sidecars_for_paths(fs, &mut entry.retry_sidecars, paths).map_err(EngineError::Other)?;
        }
        Ok(())
    }

    /// A healthy shared packet may be saved while another photo's repair is unavailable.
    /// Check its retained content too, without forcing that unrelated physical operation.
    pub(crate) fn validate_sidecar_packets_for_paths(&self, paths: &[PathBuf], fs: &dyn MoveFs) -> Result<()> {
        for repair in self.pending_sidecar_repairs.iter().chain(self.undo.iter().chain(&self.redo).flat_map(|e| &e.retry_sidecars)) {
            if paths.iter().any(|path| repair.touches(fs, path)) {
                repair.validate_repair(fs).map_err(EngineError::Other)?;
            } else {
                for packet in &repair.companions {
                    if paths.iter().any(|path| fs.same_file(path, &packet.path)) {
                        packet.validate(fs).map_err(EngineError::Other)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Only a successful application write updates a retained content version. Atomic writes
    /// replace inodes; aliases that now resolve to the written path observe the new content.
    pub(crate) fn note_sidecar_write(&mut self, path: &Path, bytes: &[u8]) {
        let version = PacketVersion::of(bytes);
        for repair in self.pending_sidecar_repairs.iter_mut().chain(self.undo.iter_mut().chain(&mut self.redo).flat_map(|e| &mut e.retry_sidecars)) {
            for packet in &mut repair.companions {
                if RealFs.same_file(path, &packet.path) {
                    packet.version = Some(version);
                }
            }
        }
    }

    /// [`Session::apply_rename`] on the file system `fs`.
    pub(crate) fn apply_rename_with(&mut self, fs: &dyn MoveFs, plans: &[RenamePlan]) -> Result<usize> {
        self.apply_file_plans(fs, plans, &|n| format!("Rename {n} Photo{}", if n == 1 { "" } else { "s" }))
    }

    /// Carry out `plans` (renames or moves to another folder): move the files with their sidecars
    /// (all or nothing), then commit one undoable op, labelled `label(count)`, that points the
    /// photos (and their virtual copies) at the new paths. Undo moves the files back.
    pub(crate) fn apply_file_plans(&mut self, fs: &dyn MoveFs, plans: &[RenamePlan], label: &dyn Fn(usize) -> String) -> Result<usize> {
        // A partly completed rename can already have the requested photo name. Repair
        // its sidecars before treating that empty move list as a successful retry.
        self.repair_sidecars_with(fs)?;
        let mut ops = Vec::new();
        let by_path: HashMap<&str, &RenamePlan> = plans.iter().filter_map(|p| p.from_path.as_deref().map(|f| (f, p))).collect();
        for p in self.catalog.photos() {
            let plan = match &p.source {
                Source::File { path } => by_path.get(path.as_str()).copied(),
                Source::Demo { .. } => plans.iter().find(|x| x.id == p.id.0 && x.from_path.is_none()),
            };
            let Some(plan) = plan else { continue };
            let source = match &plan.to_path {
                Some(t) if matches!(&p.source, Source::File { path } if Path::new(path) == Path::new(t)) => p.source.clone(),
                Some(t) => Source::File { path: t.clone() },
                None => p.source.clone(),
            };
            if plan.to != p.file_name || source != p.source {
                ops.push(Op::SetFile { id: p.id, file_name: plan.to.clone(), source });
            }
        }
        let n = ops.len();
        let op = Op::Batch { ops };
        let moves: Vec<(String, String)> = plans
            .iter()
            .filter_map(|pl| match (&pl.from_path, &pl.to_path) {
                (Some(a), Some(b)) if Path::new(a) != Path::new(b) => Some((a.clone(), b.clone())),
                _ => None,
            })
            .collect();
        if let Err(e) = move_all(fs, &moves) {
            return Err(EngineError::Other(self.follow_stuck(&op, e, true).0));
        }
        if n > 0
            && let Err(err) = self.commit(&label(n), op)
        {
            let back: Vec<(String, String)> = moves.iter().rev().map(|(a, b)| (b.clone(), a.clone())).collect();
            if let Err(e) = move_all(fs, &back) {
                return Err(EngineError::Other(format!("{err}; the renamed files could not all be moved back: {}", e.message)));
            }
            return Err(err);
        }
        Ok(n)
    }

    /// After a failed batch of moves for `op`: point the photos whose files stayed moved at
    /// their new paths (an undoable "partly" rename when `undoable`, otherwise a plain logged
    /// change), so the library matches the disk. Returns the message and, for a history retry,
    /// the correction's inverse file ops (so a retry doesn't lose the original paths).
    pub(crate) fn follow_stuck(&mut self, op: &Op, e: BatchMoveError, undoable: bool) -> (String, Vec<Op>) {
        let mut msg = e.message;
        let pending_sidecars = e.pending_sidecars;
        if e.stuck.is_empty() && (!undoable || pending_sidecars.is_empty()) {
            return (msg, Vec::new());
        }
        let to: HashSet<&str> = e.stuck.iter().map(|m| m.1.as_str()).collect();
        let mut keep = Vec::new();
        set_file_ops_to(op, &to, &mut keep);
        let list: Vec<String> = e.stuck.iter().map(|(a, b)| format!("{a} → {b}")).collect();
        let n = e.stuck.len();
        let s = if n == 1 { "" } else { "s" };
        let fix = Op::Batch { ops: keep };
        let mut inverses = Vec::new();
        let applied = if undoable {
            let label = if n == 0 { "Repair Rename Sidecars".to_string() } else { format!("Rename {n} Photo{s} (partly)") };
            self.commit(&label, fix).map(|()| {
                if let Some(entry) = self.undo.last_mut() {
                    entry.retry_sidecars.extend(pending_sidecars);
                }
            })
        } else {
            match self.apply_catalog(fix.clone()) {
                Ok(inv) => {
                    inverses = match inv {
                        Op::Batch { ops } => ops,
                        op => vec![op],
                    };
                    self.pending_log.push(fix);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        };
        match applied {
            Ok(()) if n > 0 => msg
                .push_str(&format!(". {n} file{s} could not be moved back and kept the new name; the library now points there: {}", list.join(", "))),
            Ok(()) => {}
            Err(err) => msg.push_str(&format!(
                ". {n} file{s} could not be moved back and the library could not follow ({err}); relink with Find Missing Photos: {}",
                list.join(", ")
            )),
        }
        (msg, inverses)
    }

    /// Physical moves to the final `SetFile` destination of each photo. A merged history
    /// batch can rename the same photo repeatedly; intermediate catalog paths need no file.
    pub(crate) fn file_moves(&self, op: &Op) -> Vec<(String, String)> {
        let mut pending = vec![op];
        let mut order = Vec::new();
        let mut targets = HashMap::new();
        while let Some(op) = pending.pop() {
            match op {
                Op::Batch { ops } => pending.extend(ops.iter().rev()),
                Op::SetFile { id, source, .. } if targets.insert(*id, source).is_none() => {
                    order.push(*id);
                }
                _ => {}
            }
        }
        // Virtual copies share one physical move, keeping the original batch order.
        let mut seen = HashSet::new();
        order
            .into_iter()
            .filter_map(|id| {
                let Source::File { path: to } = targets.get(&id).copied()? else { return None };
                let Source::File { path: from } = &self.catalog.photo(id)?.source else { return None };
                (Path::new(from) != Path::new(to) && seen.insert(from)).then(|| (from.clone(), to.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Windows has no file id in std: two spellings of one file are the same file whichever is
    /// asked about first (a save after a partial rename saw `Trip.XMP` as a collision with the
    /// known `Trip.xmp`), and only the spelling on disk is listed.
    #[cfg(windows)]
    #[test]
    fn same_file_spellings_are_symmetric_on_windows() {
        let dir = std::env::temp_dir().join(format!("lc-same-file-spelling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (real, alias, other) = (dir.join("Trip.xmp"), dir.join("Trip.XMP"), dir.join("other.xmp"));
        std::fs::write(&real, b"packet").unwrap();
        std::fs::write(&other, b"packet").unwrap();
        assert!(RealFs.same_file(&real, &alias) && RealFs.same_file(&alias, &real));
        assert!(!RealFs.same_file(&real, &other) && !RealFs.same_file(&other, &alias));
        assert!(RealFs.listed(&real) && !RealFs.listed(&alias));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The cross-volume move's check compares files chunk by chunk: equal files across several
    /// chunks, a difference in the last chunk, different lengths, a missing file.
    #[test]
    fn same_contents_compares_in_chunks() {
        let dir = std::env::temp_dir().join(format!("lc-same-contents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big: Vec<u8> = (0..(2usize << 20) + 123).map(|i| (i % 251) as u8).collect();
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::write(&a, &big).unwrap();
        std::fs::write(&b, &big).unwrap();
        assert!(same_contents(&a, &b).unwrap());
        let mut other = big.clone();
        *other.last_mut().unwrap() ^= 1;
        std::fs::write(&b, &other).unwrap();
        assert!(!same_contents(&a, &b).unwrap());
        std::fs::write(&b, &big[..big.len() - 1]).unwrap();
        assert!(!same_contents(&a, &b).unwrap());
        std::fs::write(&a, b"").unwrap();
        std::fs::write(&b, b"").unwrap();
        assert!(same_contents(&a, &b).unwrap());
        assert!(same_contents(&a, &dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A model volume: files by path with contents; case-insensitive (default APFS/NTFS) or
    /// case-sensitive (Linux, case-sensitive APFS). `fail` makes renames from a matching source fail.
    pub(crate) struct FakeFs {
        pub ci: bool,
        pub files: RefCell<Vec<(String, Vec<u8>)>>,
        pub fail: RefCell<Vec<String>>,
    }

    impl FakeFs {
        pub fn new(ci: bool, files: &[(&str, &str)]) -> FakeFs {
            FakeFs {
                ci,
                files: RefCell::new(files.iter().map(|(p, c)| (p.to_string(), c.as_bytes().to_vec())).collect()),
                fail: RefCell::new(Vec::new()),
            }
        }
        /// The model volume spells paths with `/`; the code under test joins them with the host's separator.
        fn norm(p: &Path) -> String {
            p.to_string_lossy().replace('\\', "/")
        }
        fn idx(&self, p: &Path) -> Option<usize> {
            let p = Self::norm(p);
            self.files.borrow().iter().position(|(f, _)| {
                let f = f.replace('\\', "/");
                if self.ci { folded_path(&f) == folded_path(&p) } else { f == p }
            })
        }
        /// The listing: (exact path, contents), sorted.
        pub fn listing(&self) -> Vec<(String, String)> {
            let mut v: Vec<_> = self.files.borrow().iter().map(|(p, c)| (p.replace('\\', "/"), String::from_utf8_lossy(c).to_string())).collect();
            v.sort();
            v
        }
    }

    impl MoveFs for FakeFs {
        fn packet_version(&self, path: &Path) -> std::io::Result<PacketVersion> {
            let i = self.idx(path).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such packet"))?;
            self.files.borrow().get(i).map(|(_, bytes)| PacketVersion::of(bytes)).ok_or_else(|| std::io::Error::other("packet disappeared"))
        }
        fn exists(&self, p: &Path) -> bool {
            self.idx(p).is_some()
        }
        fn is_file(&self, p: &Path) -> bool {
            self.exists(p)
        }
        fn same_file(&self, a: &Path, b: &Path) -> bool {
            self.idx(a).is_some() && self.idx(a) == self.idx(b)
        }
        fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
            if self.fail.borrow().iter().any(|f| Self::norm(a).contains(&f.replace('\\', "/"))) {
                return Err(std::io::Error::other("injected failure"));
            }
            if self.exists(b) {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "exists"));
            }
            let i = self.idx(a).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
            self.files.borrow_mut()[i].0 = Self::norm(b);
            Ok(())
        }
        fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
            if self.fail.borrow().iter().any(|f| Self::norm(a).contains(&f.replace('\\', "/"))) {
                return Err(std::io::Error::other("injected failure"));
            }
            if self.exists(b) {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "exists"));
            }
            let i = self.idx(a).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
            let c = self.files.borrow()[i].1.clone();
            self.files.borrow_mut().push((Self::norm(b), c));
            Ok(())
        }
        fn remove_file(&self, p: &Path) -> std::io::Result<()> {
            let i = self.idx(p).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
            self.files.borrow_mut().remove(i);
            Ok(())
        }
        fn listed(&self, p: &Path) -> bool {
            let p = Self::norm(p);
            self.files.borrow().iter().any(|(f, _)| f.replace('\\', "/") == p)
        }
        fn stem_sibling(&self, sidecar: &Path) -> Option<String> {
            let stem = Self::norm(&sidecar.with_extension(""));
            self.files.borrow().iter().map(|(f, _)| f.clone()).find(|f| {
                let p = Path::new(f);
                Self::norm(&p.with_extension("")) == stem && !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
            })
        }
    }

    fn file_photo(id: u64, path: &str) -> Photo {
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // the host's separator, so `dir.join(name)` in the planner spells the same path the same way
        let path = path.replace('/', std::path::MAIN_SEPARATOR_STR);
        Photo::new(PhotoId(id), Source::File { path }, &name, "JPG", 1, 1, "2026-01-01T00:00:00")
    }

    /// Compare the unfinished operation and every observed packet after partial progress.
    /// Companion order is incidental; paths and recorded content versions are exact.
    pub(crate) fn assert_repair_packets(actual: &SidecarMove, unfinished: &SidecarMove, packets: &[(PathBuf, &[u8])]) {
        let mut expected = unfinished.clone();
        expected.companions =
            packets.iter().map(|(path, bytes)| RetainedPacket { path: path.clone(), version: Some(PacketVersion::of(bytes)) }).collect();
        let mut actual = actual.clone();
        actual.companions.sort_by(|a, b| a.path.cmp(&b.path));
        expected.companions.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(actual, expected);
    }

    #[test]
    fn folder_moves_repair_retained_sidecars_before_changing_paths() {
        for command in ["folder.rename", "folder.move"] {
            for queue in ["pending", "undo", "redo"] {
                for (naming, copied) in [(SidecarNaming::Stem, false), (SidecarNaming::Full, false), (SidecarNaming::Stem, true)] {
                    let root = crate::tests_xmp::temp_dir("folder-sidecar-repair");
                    let from = root.join("Trip");
                    std::fs::create_dir_all(&from).unwrap();
                    let photo = from.join("b.jpg");
                    std::fs::write(&photo, b"Photo bytes").unwrap();
                    let old_sidecar = crate::sidecar::sidecar_path(&from.join("a.jpg").to_string_lossy(), naming);
                    let sidecar = crate::sidecar::sidecar_path(&photo.to_string_lossy(), naming);
                    std::fs::write(&old_sidecar, b"Retained edits").unwrap();
                    // An ordinary occupied destination must block repair without replacing its bytes.
                    std::fs::write(&sidecar, b"Other edits").unwrap();
                    let mut s = Session::new();
                    s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(1, &photo.to_string_lossy())) }).unwrap();
                    if copied {
                        let sibling = from.join("a.raw");
                        std::fs::write(&sibling, b"Shared original").unwrap();
                        s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(2, &sibling.to_string_lossy())) }).unwrap();
                    }
                    s.commit("Rating", Op::SetRating { id: PhotoId(1), rating: 3 }).unwrap();
                    if queue == "redo" {
                        s.undo_step().unwrap();
                    }
                    let repair = SidecarMove {
                        from: old_sidecar.clone(),
                        to: sidecar.clone(),
                        copied,
                        companions: vec![RetainedPacket { path: old_sidecar.clone(), version: Some(RealFs.packet_version(&old_sidecar).unwrap()) }],
                    };
                    match queue {
                        "undo" => s.undo.last_mut().unwrap().retry_sidecars.push(repair),
                        "redo" => s.redo.last_mut().unwrap().retry_sidecars.push(repair),
                        _ => s.pending_sidecar_repairs.push(repair),
                    }
                    let params = if command == "folder.rename" {
                        serde_json::json!({"path": from, "name": "Holiday"})
                    } else {
                        serde_json::json!({"path": from, "into": root.join("Archive")})
                    };
                    let to = if command == "folder.rename" { root.join("Holiday") } else { root.join("Archive").join("Trip") };
                    let before = s.catalog.to_snapshot();
                    let history = (s.undo.len(), s.redo.len());
                    assert!(s.execute(command, &params).is_err());
                    assert!(from.is_dir() && !to.exists());
                    assert_eq!(s.catalog.to_snapshot(), before);
                    assert_eq!((s.undo.len(), s.redo.len()), history);
                    assert_eq!(std::fs::read(&old_sidecar).unwrap(), b"Retained edits");
                    assert_eq!(std::fs::read(&sidecar).unwrap(), b"Other edits");
                    std::fs::remove_file(&sidecar).unwrap();
                    s.execute(command, &params).unwrap();
                    assert!(s.pending_sidecar_repairs.is_empty());
                    assert!(s.undo.iter().chain(&s.redo).all(|e| e.retry_sidecars.is_empty()));
                    let moved_sidecar = to.join(sidecar.file_name().unwrap());
                    assert_eq!(std::fs::read(&moved_sidecar).unwrap(), b"Retained edits");
                    assert_eq!(to.join(old_sidecar.file_name().unwrap()).exists(), copied);
                    for (step, current) in [("edit.undo", &from), ("edit.redo", &to)] {
                        s.execute(step, &serde_json::json!({})).unwrap();
                        assert_eq!(
                            s.catalog.photo(PhotoId(1)).unwrap().source,
                            Source::File { path: current.join("b.jpg").to_string_lossy().into_owned() }
                        );
                        assert_eq!(std::fs::read(current.join(sidecar.file_name().unwrap())).unwrap(), b"Retained edits");
                        assert_eq!(current.join(old_sidecar.file_name().unwrap()).exists(), copied);
                    }
                    // Explicit IDs choose targets after the command's selection gate.
                    s.execute("library.select", &serde_json::json!({"ids": [1], "active": 1})).unwrap();
                    s.execute("photo.rename", &serde_json::json!({"ids": [1], "template": "Latest"})).unwrap();
                    s.execute("edit.undo", &serde_json::json!({})).unwrap();
                    assert!(to.join("b.jpg").is_file());
                    assert_eq!(std::fs::read(&moved_sidecar).unwrap(), b"Retained edits");
                    s.execute("edit.redo", &serde_json::json!({})).unwrap();
                    assert!(to.join("Latest.jpg").is_file());
                    let final_sidecar = crate::sidecar::sidecar_path(&to.join("Latest.jpg").to_string_lossy(), naming);
                    assert_eq!(std::fs::read(final_sidecar).unwrap(), b"Retained edits");
                    std::fs::remove_dir_all(root).unwrap();
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn mixed_separator_noop_rename_preserves_files_catalog_and_undo() {
        let fs = FakeFs::new(true, &[("N:/photos/IMG.jpg", "image"), ("N:/photos/IMG.xmp", "edits"), ("N:/photos/other.jpg", "other")]);
        // Any attempt to move the unchanged photo (including rollback) must fail.
        fs.fail.borrow_mut().push("IMG".into());
        let mut s = Session::new();
        // spelled with `/` on purpose (not `file_photo`, which uses the host's separator)
        for (id, path) in [(1, "N:/photos/IMG.jpg"), (2, "N:/photos/other.jpg")] {
            let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let photo = Photo::new(PhotoId(id), Source::File { path: path.into() }, &name, "JPG", 1, 1, "2026-01-01T00:00:00");
            s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        let before = fs.listing();
        let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "{name}", 1);
        assert_ne!(plans[0].from_path, plans[0].to_path, "fixture exercises different separator spelling");
        assert_eq!(s.apply_rename_with(&fs, &plans).unwrap(), 0);
        assert_eq!(fs.listing(), before);
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path: "N:/photos/IMG.jpg".into() });
        assert!(s.undo.is_empty());
        move_file_with(&fs, "N:/photos/IMG.jpg", "N:/photos\\IMG.jpg").unwrap();
        let separator_only = Op::SetFile { id: PhotoId(1), file_name: "IMG.jpg".into(), source: Source::File { path: "N:/photos\\IMG.jpg".into() } };
        assert!(s.file_moves(&separator_only).is_empty());

        // A batch may combine that no-op with a genuine rename, without touching IMG or its sidecar.
        let mut batch = plans;
        batch.extend(s.plan_rename_with(&fs, &[PhotoId(2)], "renamed", 1));
        assert_eq!(s.apply_rename_with(&fs, &batch).unwrap(), 1);
        assert_eq!(
            fs.listing(),
            vec![
                ("N:/photos/IMG.jpg".into(), "image".into()),
                ("N:/photos/IMG.xmp".into(), "edits".into()),
                ("N:/photos/renamed.jpg".into(), "other".into())
            ]
        );
        let undo = &s.undo.last().unwrap().op;
        let moves = s.file_moves(undo);
        assert_eq!(moves.len(), 1);
        assert_eq!(Path::new(&moves[0].1), Path::new("N:/photos/other.jpg"));
        move_all(&fs, &moves).unwrap();
        s.catalog.apply(undo.clone()).unwrap();
        assert_eq!(fs.listing(), before);
    }

    /// Issue #95: a case-only rename is a real change on a case-insensitive volume and never
    /// replaces a different file that differs only by case on a case-sensitive one.
    #[test]
    fn case_only_renames_never_replace_another_file() {
        // case-sensitive: IMG_1.JPG and img_1.JPG are two photos
        let fs = FakeFs::new(false, &[("/p/IMG_1.JPG", "first"), ("/p/img_1.JPG", "second"), ("/p/IMG_1.xmp", "sc1"), ("/p/img_1.xmp", "sc2")]);
        assert!(move_file_with(&fs, "/p/IMG_1.JPG", "/p/img_1.JPG").unwrap_err().message.contains("already exists"));
        assert_eq!(fs.listing().len(), 4);
        assert!(fs.listing().contains(&("/p/img_1.JPG".into(), "second".into())));
        // the planner picks a free name instead
        let mut s = Session::new();
        s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(1, "/p/IMG_1.JPG")) }).unwrap();
        let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "img_1", 1);
        assert_eq!(plans[0].to, "img_1-1.JPG");
        move_file_with(&fs, "/p/IMG_1.JPG", plans[0].to_path.as_deref().unwrap()).unwrap();
        assert_eq!(
            fs.listing(),
            vec![
                ("/p/img_1-1.JPG".into(), "first".into()),
                ("/p/img_1-1.xmp".into(), "sc1".into()),
                ("/p/img_1.JPG".into(), "second".into()),
                ("/p/img_1.xmp".into(), "sc2".into())
            ]
        );
        // case-sensitive, target free: a plain rename
        let fs = FakeFs::new(false, &[("/p/IMG_2.JPG", "x")]);
        move_file_with(&fs, "/p/IMG_2.JPG", "/p/img_2.JPG").unwrap();
        assert_eq!(fs.listing(), vec![("/p/img_2.JPG".into(), "x".into())]);

        // case-insensitive: the "existing" target is the photo itself → renamed via a temp name
        let fs = FakeFs::new(true, &[("/p/IMG_1.JPG", "first"), ("/p/IMG_1.xmp", "sc1")]);
        let mut s = Session::new();
        s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(1, "/p/IMG_1.JPG")) }).unwrap();
        let plans = s.plan_rename_with(&fs, &[PhotoId(1)], "img_1", 1);
        assert_eq!(plans[0].to, "img_1.JPG", "not a collision with itself");
        move_file_with(&fs, "/p/IMG_1.JPG", "/p/img_1.JPG").unwrap();
        assert_eq!(fs.listing(), vec![("/p/img_1.JPG".into(), "first".into()), ("/p/img_1.xmp".into(), "sc1".into())]);
        // case-insensitive, another file under another name: still a collision
        let fs = FakeFs::new(true, &[("/p/A.JPG", "a"), ("/p/b.JPG", "b")]);
        assert!(move_file_with(&fs, "/p/A.JPG", "/p/B.JPG").is_err());
    }

    /// On a case-insensitive volume (Windows, macOS) `a.XMP` answers for `a.xmp`: a renamed
    /// photo's sidecars keep the spelling they have on disk, and a failure on the real spelling
    /// is the one reported and queued.
    #[test]
    fn case_insensitive_renames_keep_each_sidecars_spelling() {
        let fs = FakeFs::new(true, &[("/p/a.jpg", "A"), ("/p/a.xmp", "stem"), ("/p/a.jpg.XMP", "full"), ("/p/b.jpg", "B"), ("/p/b.Xmp", "mixed")]);
        move_file_with(&fs, "/p/a.jpg", "/p/c.jpg").unwrap();
        move_file_with(&fs, "/p/b.jpg", "/p/d.jpg").unwrap();
        assert_eq!(
            fs.listing(),
            vec![
                ("/p/c.jpg".into(), "A".into()),
                ("/p/c.jpg.XMP".into(), "full".into()),
                ("/p/c.xmp".into(), "stem".into()),
                ("/p/d.jpg".into(), "B".into()),
                ("/p/d.xmp".into(), "mixed".into())
            ]
        );
        // the move of the sidecar's real spelling is the one attempted (and here fails): the photo
        // goes back and nothing changed
        let before = fs.listing();
        *fs.fail.borrow_mut() = vec!["/p/c.xmp".into()];
        let e = move_file_with(&fs, "/p/c.jpg", "/p/a.jpg").unwrap_err();
        assert!(e.message.replace('\\', "/").contains("rename /p/c.xmp: injected failure"), "{e}");
        assert!(!e.moved && e.pending_sidecars.is_empty());
        assert_eq!(fs.listing(), before);
    }

    /// Issue #105: a rollback that fails is reported, and the library follows the files that kept
    /// their new names (an undoable partial rename), so nothing goes missing.
    #[test]
    fn failed_rollbacks_are_reported_and_followed() {
        let fs = FakeFs::new(false, &[("/p/a.jpg", "A"), ("/p/a.xmp", "As"), ("/p/b.jpg", "B"), ("/p/c.jpg", "C")]);
        let mut s = Session::new();
        for (i, f) in ["/p/a.jpg", "/p/b.jpg", "/p/c.jpg"].iter().enumerate() {
            s.catalog.apply(Op::AddPhoto { photo: Box::new(file_photo(i as u64 + 1, f)) }).unwrap();
        }
        let ids = [PhotoId(1), PhotoId(2), PhotoId(3)];
        let plans = s.plan_rename_with(&fs, &ids, "Trip-{seq}", 1);
        // c can't be renamed (the share went away), and neither can Trip-1 be moved back
        fs.fail.borrow_mut().extend(["c.jpg".to_string(), "Trip-1".to_string()]);
        let err = s.apply_rename_with(&fs, &plans).unwrap_err().to_string().replace('\\', "/");
        assert!(err.contains("rename /p/c.jpg"), "{err}");
        assert!(err.contains("/p/Trip-1.jpg could not be moved back"), "{err}");
        assert!(err.contains("/p/a.jpg → /p/Trip-1.jpg"), "lists what stayed renamed: {err}");
        assert_eq!(
            fs.listing(),
            vec![
                ("/p/Trip-1.jpg".into(), "A".into()),
                ("/p/Trip-1.xmp".into(), "As".into()),
                ("/p/b.jpg".into(), "B".into()),
                ("/p/c.jpg".into(), "C".into())
            ]
        );
        // the catalog matches the disk
        let path = |s: &Session, id: u64| match &s.catalog.photo(PhotoId(id)).unwrap().source {
            Source::File { path } => path.replace('\\', "/"),
            Source::Demo { .. } => String::new(),
        };
        assert_eq!(path(&s, 1), "/p/Trip-1.jpg");
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().file_name, "Trip-1.jpg");
        assert_eq!((path(&s, 2), path(&s, 3)), ("/p/b.jpg".into(), "/p/c.jpg".into()));
        assert_eq!(s.undo.last().map(|e| e.label.as_str()), Some("Rename 1 Photo (partly)"));

        // a sidecar that won't move, and a file that won't go back: the file keeps its new name
        // with the sidecars that did move, and says so
        let fs = FakeFs::new(false, &[("/p/d.jpg", "D"), ("/p/d.xmp", "Ds"), ("/p/d.jpg.xmp", "Df")]);
        fs.fail.borrow_mut().extend(["d.jpg.xmp".to_string(), "/p/e.jpg".to_string()]);
        let e = move_file_with(&fs, "/p/d.jpg", "/p/e.jpg").unwrap_err();
        assert!(e.moved, "{e}");
        assert!(e.message.replace('\\', "/").contains("it is now /p/e.jpg with its sidecar /p/e.xmp"), "{e}");
        assert_eq!(fs.listing(), vec![("/p/d.jpg.xmp".into(), "Df".into()), ("/p/e.jpg".into(), "D".into()), ("/p/e.xmp".into(), "Ds".into())]);
        // … and when the file does go back, its sidecars follow it
        let fs = FakeFs::new(false, &[("/p/d.jpg", "D"), ("/p/d.xmp", "Ds"), ("/p/d.jpg.xmp", "Df")]);
        fs.fail.borrow_mut().push("d.jpg.xmp".to_string());
        let e = move_file_with(&fs, "/p/d.jpg", "/p/e.jpg").unwrap_err();
        assert!(!e.moved);
        assert_eq!(fs.listing().len(), 3);
        assert!(fs.listing().iter().all(|(p, _)| p.starts_with("/p/d.")));
    }

    #[test]
    fn pending_sidecars_preserve_shared_copies_and_do_not_repeat_case_aliases() {
        // These are synthetic volume semantics; no host driver or OS runtime is involved.
        for ci in [false, true] {
            let fs = FakeFs::new(ci, &[("/p/a.CR3", "Raw"), ("/p/a.JPG", "Jpeg"), ("/p/a.XMP", "Shared edits"), ("/p/a.CR3.XMP", "Raw edits")]);
            fs.fail.borrow_mut().extend(["/p/a.XMP".into(), "/p/b.CR3".into()]);
            let mut error = move_file_with(&fs, "/p/a.CR3", "/p/b.CR3").unwrap_err();
            assert!(error.moved);
            assert_eq!(error.pending_sidecars.len(), 2);
            assert!(error.pending_sidecars[0].copied);
            assert!(!error.pending_sidecars[1].copied);
            fs.fail.borrow_mut().clear();
            retry_sidecars(&fs, &mut error.pending_sidecars).unwrap();
            assert!(error.pending_sidecars.is_empty());
            assert_eq!(
                fs.listing(),
                vec![
                    ("/p/a.JPG".into(), "Jpeg".into()),
                    ("/p/a.XMP".into(), "Shared edits".into()),
                    ("/p/b.CR3".into(), "Raw".into()),
                    ("/p/b.CR3.XMP".into(), "Raw edits".into()),
                    ("/p/b.XMP".into(), "Shared edits".into()),
                ]
            );
        }
    }

    /// Issue #92 (comment): renaming one of `IMG_1.CR3` + `IMG_1.JPG` copies their shared
    /// `IMG_1.xmp` instead of taking it away from the other; the last one renamed takes it.
    #[test]
    fn shared_stem_sidecar_is_copied_not_taken() {
        let fs = FakeFs::new(false, &[("/p/IMG_1.CR3", "raw"), ("/p/IMG_1.JPG", "jpg"), ("/p/IMG_1.xmp", "shared"), ("/p/IMG_1.CR3.xmp", "own")]);
        move_file_with(&fs, "/p/IMG_1.CR3", "/p/Trip_1.CR3").unwrap();
        assert_eq!(
            fs.listing(),
            vec![
                ("/p/IMG_1.JPG".into(), "jpg".into()),
                ("/p/IMG_1.xmp".into(), "shared".into()),
                ("/p/Trip_1.CR3".into(), "raw".into()),
                ("/p/Trip_1.CR3.xmp".into(), "own".into()),
                ("/p/Trip_1.xmp".into(), "shared".into()),
            ]
        );
        // the JPEG is the last one using it: it moves
        move_file_with(&fs, "/p/IMG_1.JPG", "/p/Trip_2.JPG").unwrap();
        assert!(fs.listing().contains(&("/p/Trip_2.xmp".into(), "shared".into())));
        assert!(!fs.exists(Path::new("/p/IMG_1.xmp")));
        // a failure after the copy removes the copy and keeps the original
        let fs = FakeFs::new(false, &[("/p/A.CR3", "raw"), ("/p/A.JPG", "jpg"), ("/p/A.xmp", "shared"), ("/p/A.CR3.xmp", "own")]);
        fs.fail.borrow_mut().push("A.CR3.xmp".into());
        assert!(move_file_with(&fs, "/p/A.CR3", "/p/B.CR3").is_err());
        assert_eq!(fs.listing().len(), 4);
        assert!(fs.listing().contains(&("/p/A.xmp".into(), "shared".into())) && !fs.exists(Path::new("/p/B.xmp")));
    }

    /// Issue #95 on the real disk, whichever kind of volume the temp folder is on.
    #[test]
    fn case_only_rename_on_disk() {
        let dir = std::env::temp_dir().join(format!("lc-rename-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (upper, lower) = (dir.join("IMG_1.JPG"), dir.join("img_1.JPG"));
        std::fs::write(&upper, b"first").unwrap();
        let case_sensitive = !lower.exists();
        let p = |q: &Path| q.to_string_lossy().to_string();
        if case_sensitive {
            std::fs::write(&lower, b"second").unwrap();
            assert!(move_file(&p(&upper), &p(&lower)).is_err());
            assert_eq!(std::fs::read(&lower).unwrap(), b"second");
            assert_eq!(std::fs::read(&upper).unwrap(), b"first");
        } else {
            move_file(&p(&upper), &p(&lower)).unwrap();
            let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
            assert_eq!(names, vec!["img_1.JPG".to_string()]);
            assert_eq!(std::fs::read(&lower).unwrap(), b"first");
        }
        // a plain rename never replaces
        std::fs::write(dir.join("other.JPG"), b"other").unwrap();
        assert!(move_file(&p(&dir.join("other.JPG")), &p(&lower)).is_err());
        assert_eq!(std::fs::read(dir.join("other.JPG")).unwrap(), b"other");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_templates_stay_inside_the_destination() {
        let mut p = sample_photo();
        assert_eq!(expand_folder("{date:%Y}/{date:%Y%m%d}", &p, 1), vec!["2026", "20260114"]);
        assert_eq!(expand_folder("{date:%Y}\\{date:%Y-%m}", &p, 1), vec!["2026", "2026-01"], "backslash separates too");
        // metadata can't add levels or climb out
        p.meta.camera = "../../etc/x".into();
        p.meta.title = "..".into();
        assert_eq!(expand_folder("{camera}/{title}/{date:%Y}", &p, 1), vec!["-..-etc-x", "unknown", "2026"]);
        p.meta.camera = "C:\\Windows".into();
        assert_eq!(expand_folder("{camera}", &p, 1), vec!["C--Windows"]);
        // missing metadata → `unknown`; missing capture time → the import time
        p.meta.camera.clear();
        p.captured = None;
        assert_eq!(expand_folder("{camera}//{date:%Y%m%d}", &p, 1), vec!["unknown", "20260120"]);
        for bad in ["", "  ", "/abs/{date}", "\\\\server\\x", "C:/x", "c:x", "~/x", "../{date}", "{date}/../x", "a/./b", "{date:%Y}/.."] {
            assert!(folder_template_error(bad).is_some(), "{bad:?} should be rejected");
        }
        for ok in ["{date:%Y}/{date:%Y%m%d}", "Trips/{camera}", "{date}", "a//b"] {
            assert_eq!(folder_template_error(ok), None, "{ok:?}");
        }
        // even unchecked, rejected levels are skipped rather than followed
        assert_eq!(expand_folder("/../{date:%Y}/./x", &p, 1), vec!["2026", "x"]);
    }

    /// The help lists exactly what `expand_tokens` understands: every listed tag (and alias) is
    /// replaced, never left literal, and the help's examples match the documented behaviour.
    #[test]
    fn token_help_matches_the_implementation() {
        let p = sample_photo();
        for t in TOKENS {
            for tag in std::iter::once(&t.tag).chain(t.aliases) {
                let v = expand_tokens(tag, &p, 1, 1);
                assert!(!v.contains('{'), "{tag} is not a known token (expanded to {v:?})");
                assert!(!v.is_empty(), "{tag}: the sample photo should give an example");
            }
        }
        assert_eq!(token_example("{seq:3}"), "001");
        assert_eq!(token_example("{date}"), "20260114");
        assert_eq!(token_example("{date:%Y%m%d_%H%M%S}"), "20260114_055848");
        assert_eq!(token_example("{ext}"), "CR3");
        assert_eq!(expand("{date:%Y%m%d_%H%M%S}_{seq:3}", &p, 1), "20260114_055848_001.CR3");
        for (d, _) in DATE_DIRECTIVES {
            let v = format_date("2026-01-14T05:58:48", d);
            assert!(!v.contains('%') || *d == "%%", "{d} is not a known directive");
        }
        assert_eq!(unknown_tokens("{date}_{camra}-{seq:2}{x}{camra}"), vec!["{camra}".to_string(), "{x}".to_string()]);
        assert!(unknown_tokens("{name}{ext}{date:%Y}").is_empty());
        let json = token_help_json();
        assert_eq!(json["tokens"].as_array().unwrap().len(), TOKENS.len());
        assert_eq!(json["tokens"][0]["example"], "IMG_0042");
        // the command descriptions that list the tokens list all of them
        for id in ["photo.rename", "photo.renamePreview", "library.import"] {
            let spec = crate::cmd::command_specs().iter().find(|c| c.id == id).unwrap();
            for t in TOKENS.iter().filter(|t| !t.tag.contains(':')) {
                assert!(spec.params.contains(t.tag), "{id} does not mention {}", t.tag);
            }
        }
    }

    #[test]
    fn templates() {
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 1 }, "IMG_0042.CR2", "CR2", 1, 1, "2026-01-01T00:00:00");
        p.captured = Some("2026-09-30T14:05:09".into());
        p.meta.camera = "Model X/2".into();
        p.meta.title = "Sunset: beach".into();
        assert_eq!(expand("{name}", &p, 1), "IMG_0042.CR2");
        assert_eq!(expand("Trip-{seq:3}", &p, 7), "Trip-007.CR2");
        assert_eq!(expand("{date}_{name}", &p, 1), "20260930_IMG_0042.CR2");
        assert_eq!(expand("{date:%Y-%m-%d %H.%M.%S}", &p, 1), "2026-09-30 14.05.09.CR2");
        assert_eq!(expand("{camera} {title}", &p, 1), "Model X-2 Sunset- beach.CR2");
        assert_eq!(expand("{unknown}-{seq}", &p, 12), "{unknown}-12.CR2");
        assert_eq!(expand("  ", &p, 1), "IMG_0042.CR2", "empty template keeps the name");
        assert_eq!(expand("../{name}", &p, 1), "-IMG_0042.CR2", "no path components");
    }

    #[test]
    fn number_folder_and_metadata_tokens() {
        let path = std::path::Path::new("shoots").join("2026-09 Coast").join("DSC_0815.NEF");
        let mut p = Photo::new(PhotoId(1), Source::File { path: path.to_string_lossy().into() }, "DSC_0815.NEF", "NEF", 1, 1, "2026-01-01T00:00:00");
        p.meta.lens = "24-70mm F2.8".into();
        p.meta.iso = Some(400);
        p.meta.creator = "A. Person".into();
        p.rating = 4;
        assert_eq!(expand("{folder}_{num}", &p, 1), "2026-09 Coast_0815.NEF");
        assert_eq!(expand("{rating}star-{iso}-{lens}-{creator}", &p, 1), "4star-400-24-70mm F2.8-A. Person.NEF");
        // no number at the end of the name / no folder: empty
        p.file_name = "beach.NEF".into();
        p.source = Source::Demo { scene: 0 };
        assert_eq!(expand("{folder}{name}{num}", &p, 1), "beach.NEF");
        // a bare {seq} is padded to the caller's width
        assert_eq!(expand_tokens("{seq}|{seq:2}", &p, 7, 3), "007|07");
    }
}
