//! A variant's keywords (`ZVARIANTMETADATA.ZCONTENT_KEYWORDS`).
//!
//! The field lists entries separated by commas. Each is a keyword's path with `|` between its
//! levels and an order number after `||`: `Nature||0,Nature|Water||1`. A keyword's parents are
//! listed as entries of their own. How a comma or `|` inside a keyword's name is written has not
//! been observed; such a name is read as the field's syntax says.

use std::collections::HashSet;

/// Most keywords kept for one photo.
pub(super) const MAX_KEYWORDS: usize = 10_000;

/// The keywords of a field as `parent|child` paths in the field's order, each once, without the
/// parents that only come with a child. The flag says the field held more than
/// [`MAX_KEYWORDS`] and was cut.
pub(super) fn parse(field: &str) -> (Vec<String>, bool) {
    let mut entries: Vec<(u64, usize, String)> = Vec::new();
    let mut seen = HashSet::new();
    let mut cut = false;
    for (position, entry) in field.split(',').enumerate() {
        let Some((order, path)) = entry_path(entry) else { continue };
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
