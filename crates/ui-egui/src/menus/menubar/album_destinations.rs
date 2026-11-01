//! Add-to-album destinations in the sidebar's folder and custom sibling order.

use crate::widgets::album_picker::{AlbumEntry, entries_from};

use super::{MenuNode, item, json};

/// Reuse the picker's bounded traversal, then prune smart albums and folders with no destination.
pub(super) fn nodes(cat: &lightcraft_catalog::Catalog, enabled: bool) -> Vec<MenuNode> {
    let entries = entries_from(cat, |_| None);
    level(&entries, &mut 0, 0, enabled)
}

fn level(entries: &[AlbumEntry], at: &mut usize, depth: usize, enabled: bool) -> Vec<MenuNode> {
    if depth > 64 {
        return Vec::new();
    }
    let mut out = Vec::new();
    while let Some(e) = entries.get(*at).filter(|e| e.depth == depth) {
        *at = at.saturating_add(1);
        if e.folder {
            let children = level(entries, at, depth + 1, enabled);
            if !children.is_empty() {
                out.push(MenuNode::Submenu { id: Some(format!("albumFolder:{}", e.id)), label: e.name.clone(), children });
            }
        } else if !e.smart {
            out.push(item("album.addPhotos", json!({"id": e.id}), e.name.clone(), None, enabled, None));
        }
    }
    out
}
