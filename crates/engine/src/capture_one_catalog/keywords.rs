//! A variant's keywords (`ZVARIANTMETADATA.ZCONTENT_KEYWORDS`).
//!
//! The field lists entries separated by commas. Each is a keyword's path with `|` between its
//! levels and an order number after `||`: `Nature||0,Nature|Water||1`. A keyword's parents are
//! listed as entries of their own. How a comma or `|` inside a keyword's name is written has not
//! been observed; such a name is read as the field's syntax says.

use std::collections::{HashMap, HashSet};

use super::notice::{self, Notice};
use super::tables::Db;
use super::{Keyword, ReadError};

/// Most keywords kept for one photo.
pub(super) const MAX_KEYWORDS: usize = 10_000;

/// Deepest keyword nesting followed, in a photo's field and in the keyword list; a deeper chain
/// is a loop or damage.
pub(super) const MAX_DEPTH: usize = 64;

/// The keywords of a field as `parent|child` paths in the field's order, each once, without the
/// parents that only come with a child. The flag says the field held more than
/// [`MAX_KEYWORDS`], or a path deeper than [`MAX_DEPTH`] levels, and was cut: such a path is left
/// out, since finding its parents would take time that grows with the square of its length.
pub(super) fn parse(field: &str) -> (Vec<String>, bool) {
    let mut entries: Vec<(u64, usize, String)> = Vec::new();
    let mut seen = HashSet::new();
    let mut cut = false;
    for (position, entry) in field.split(',').enumerate() {
        let Some((order, path)) = entry_path(entry) else { continue };
        if path.split('|').nth(MAX_DEPTH).is_some() {
            cut = true;
            continue;
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        if entries.len() >= MAX_KEYWORDS {
            cut = true;
            break;
        }
        entries.push((order.unwrap_or(u64::MAX), position, path));
    }
    // every proper prefix of a path is a parent some entry implies
    let mut parents: HashSet<&str> = HashSet::new();
    for (_, _, path) in &entries {
        parents.extend(path.match_indices('|').map(|(at, _)| path.get(..at).unwrap_or("")));
    }
    let mut kept: Vec<_> = entries.iter().filter(|(_, _, path)| !parents.contains(path.as_str())).collect();
    kept.sort_by_key(|(order, position, _)| (*order, *position));
    (kept.into_iter().map(|(_, _, path)| path.clone()).collect(), cut)
}

/// An entry's order number and its path with empty levels dropped; `None` for an empty entry.
fn entry_path(entry: &str) -> Option<(Option<u64>, String)> {
    let (path, order) = match entry.rsplit_once("||") {
        Some((path, digits)) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => (path, digits.parse().ok()),
        _ => (entry, None),
    };
    let levels: Vec<&str> = path.split('|').map(str::trim).filter(|level| !level.is_empty()).collect();
    (!levels.is_empty()).then(|| (order, levels.join("|")))
}

/// The catalog's keyword list (`ZKEYWORD`): each keyword's path through its parents, in the
/// catalog's order (`ZLEFT`, a nested set), parents before children. Rows of another entity
/// (`Synonym` is one) are not keywords; how Capture One stores synonyms has not been observed, so
/// they are left out. A keyword whose parents loop is left out with a warning.
pub(super) fn list(db: &Db, warnings: &mut Vec<Notice>) -> Result<Vec<Keyword>, ReadError> {
    let table = db.optional("ZKEYWORD", warnings)?;
    let entities = db.optional("ZENTITIES", warnings)?;
    let keyword_entity = entities.rows().find(|r| r.text("ZNAME") == "Keyword").and_then(|r| r.id("Z_ENT"));
    let rows: HashMap<i64, _> = table
        .rows()
        .filter(|r| keyword_entity.is_none_or(|e| r.int("Z_ENT").is_none_or(|own| own == e)))
        .filter_map(|r| Some((r.id("Z_PK")?, r)))
        .collect();
    let mut ordered: Vec<_> = rows.iter().map(|(id, r)| (r.int("ZLEFT").unwrap_or(i64::MAX), *id)).collect();
    ordered.sort_unstable();
    let (mut out, mut seen, mut looped) = (Vec::new(), HashSet::new(), 0usize);
    for (_, id) in ordered {
        let mut levels = Vec::new();
        let (mut at, mut visited) = (Some(id), HashSet::new());
        while let Some(current) = at {
            let Some(row) = rows.get(&current) else { break };
            if !visited.insert(current) || visited.len() > MAX_DEPTH {
                levels.clear();
                looped = looped.saturating_add(1);
                break;
            }
            let name = row.text("ZNAME").trim().replace('|', " ");
            if !name.is_empty() {
                levels.push(name);
            }
            at = row.id("ZPARENT");
        }
        if levels.is_empty() {
            continue;
        }
        levels.reverse();
        let path = levels.join("|");
        let exportable = rows.get(&id).and_then(|r| r.int("ZISEXPORTABLE")).is_none_or(|n| n != 0);
        if seen.insert(path.to_lowercase()) {
            out.push(Keyword { path, exportable });
        }
    }
    if looped > 0 {
        warnings.push(Notice::new(notice::KEYWORD_LOOPS).count("n", looped));
    }
    Ok(out)
}
