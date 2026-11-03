//! Find Duplicate Photos: `library.findDuplicates` (Photo ▸ Find Duplicate Photos) and the per-frame
//! `duplicates.pump` that keeps the background job going; `library.deleteDuplicates` is what a reviewed group
//! settles on. The job itself is `crate::dupes`.

use std::collections::HashSet;

use lightcraft_catalog::{Flag, Op, PhotoId, keywords};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd};
use crate::{Result, Session};

#[cfg(not(target_arch = "wasm32"))]
fn find(s: &mut Session, p: &Value) -> Result<Value> {
    crate::dupes::find(s, p)
}

#[cfg(target_arch = "wasm32")]
fn find(_: &mut Session, _: &Value) -> Result<Value> {
    Err(bad("library.findDuplicates", "finding duplicates runs on a worker thread, which the browser build has no room for yet"))
}

#[cfg(not(target_arch = "wasm32"))]
fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    crate::dupes::pump(s, p)
}

#[cfg(target_arch = "wasm32")]
fn pump(_: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"active": false}))
}

/// The photo ids listed under `key` of `p` (none when it is missing).
fn id_list(p: &Value, key: &str, c: &str) -> Result<Vec<PhotoId>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => {
            a.iter().map(|v| v.as_u64().map(PhotoId).ok_or_else(|| bad(c, format!("`{key}` must be a list of photo ids")))).collect()
        }
        Some(_) => Err(bad(c, format!("`{key}` must be a list of photo ids"))),
    }
}

/// The groups `library.deleteDuplicates` was given, each as (kept, deleted): `groups: [{keep, delete}]`, or one
/// group from the top-level `keep` and `delete`.
fn settled_groups(p: &Value, c: &str) -> Result<Vec<(Vec<PhotoId>, Vec<PhotoId>)>> {
    match p.get("groups") {
        None | Some(Value::Null) => Ok(vec![(id_list(p, "keep", c)?, id_list(p, "delete", c)?)]),
        Some(Value::Array(gs)) => gs.iter().map(|g| Ok((id_list(g, "keep", c)?, id_list(g, "delete", c)?))).collect(),
        Some(_) => Err(bad(c, "`groups` must be a list of {keep, delete}")),
    }
}

/// `library.deleteDuplicates`: the copies a reviewed group deletes go to Recently Deleted, and what they carried —
/// keywords, a higher rating, a pick flag, a colour label — goes onto the photos their group keeps first, all as
/// one undo step. Nothing on disk changes.
fn delete_duplicates(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.deleteDuplicates";
    let groups = settled_groups(p, C)?;
    let mut seen: HashSet<PhotoId> = HashSet::new();
    for id in groups.iter().flat_map(|(keep, delete)| keep.iter().chain(delete)) {
        if !seen.insert(*id) {
            return Err(bad(C, format!("photo {} is named twice", id.0)));
        }
        if !s.catalog.photo(*id).is_some_and(|ph| ph.in_library()) {
            return Err(bad(C, format!("photo {} is not in the library", id.0)));
        }
    }
    let deleted: Vec<PhotoId> = groups.iter().flat_map(|(_, delete)| delete.iter().copied()).collect();
    if deleted.is_empty() {
        return Err(bad(C, "`delete` names no photo"));
    }
    let before = s.visible_cloned();
    let active = s.selection.active;
    // an open drag ends first, as its own undo step (as `photo.delete` does)
    s.end_interaction()?;
    let mut ops = Vec::new();
    let (mut kept, mut added_keywords, mut ratings, mut flags, mut labels) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for (keep, delete) in &groups {
        let dropped: Vec<_> = delete.iter().filter_map(|id| s.catalog.photo(*id).cloned()).collect();
        for ph in keep.iter().filter_map(|id| s.catalog.photo(*id).cloned()) {
            kept += 1;
            let mut meta = ph.meta.clone();
            let mut added = false;
            for k in dropped.iter().flat_map(|d| d.meta.keywords.iter()) {
                if !meta.keywords.iter().any(|x| keywords::same(&keywords::clean(x), &keywords::clean(k))) {
                    meta.keywords.push(k.clone());
                    added = true;
                    added_keywords += 1;
                }
            }
            if added {
                ops.push(Op::SetMeta { id: ph.id, meta: Box::new(meta) });
            }
            let rating = dropped.iter().map(|d| d.rating).max().unwrap_or(0);
            if rating > ph.rating {
                ops.push(Op::SetRating { id: ph.id, rating });
                ratings += 1;
            }
            if ph.flag == Flag::None && dropped.iter().any(|d| d.flag == Flag::Pick) {
                ops.push(Op::SetFlag { id: ph.id, flag: Flag::Pick });
                flags += 1;
            }
            if ph.label.is_none()
                && let Some(label) = dropped.iter().find_map(|d| d.label)
            {
                ops.push(Op::SetLabel { id: ph.id, label: Some(label) });
                labels += 1;
            }
        }
    }
    ops.extend(deleted.iter().map(|id| Op::SetDeleted { id: *id, deleted: true }));
    // (and out of their stacks, as `photo.delete` does, so leftover members don't keep showing `i/n`)
    ops.extend(s.catalog.remove_from_stacks_ops(&deleted));
    s.commit("Delete Duplicates", Op::Batch { ops })?;
    super::library::selection::select_after_deletion(s, &before);
    // (the mask or spot picked on the deleted photo must not stand for the one the selection moved to)
    if s.selection.active != active {
        s.active_mask = None;
        s.active_spot = None;
    }
    Ok(json!({"deleted": deleted.len(), "kept": kept, "keywords": added_keywords, "ratings": ratings, "flags": flags, "labels": labels}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        // (no menu path: the Photo menu offers the review window, `dialog.findDuplicates`; this is the command behind it,
        // and the menu would list it a second time)
        cmd!(
            "library.findDuplicates",
            "Find Duplicate Photos",
            [],
            None,
            "{ids?, distance?: 0..16 (6), sameMoment?: bool (false), filter?: bool (true), selectExtras?: bool (true), wait?: bool (false), replace?: bool (false)} → {queued, running, last?: {groups: [{exact, keep: id, delete: [id], maxDistance, photos: [{id, keep, why?}]}], photos, decoded, unreadable, duplicates, seconds, cancelled}} — photos that are the same picture (the same file twice, an export beside its raw, a resized or re-saved copy): byte-identical files, and files whose perceptual hashes differ by at most `distance` bits with aspect ratios within 3 %; a photo joins a group only when it is within `distance` of every member, so a burst can't chain into one group (`maxDistance`: the most bits two of the group's hashes differ in, over both hashes). `sameMoment` also requires the same capture time (to the second) when both photos have one: copies keep the original's, the next frame of a burst doesn't (byte-identical files are not asked: the same bytes are a copy by definition). A photo is hashed as it looks now, edits included: an edited photo no longer matches the original it came from, and a photo edited since the last run is hashed again. Each group is ranked by quality — raw over rendered, lossless over lossy, more pixels, a larger file, then edits and rating; the earlier import wins a tie — and its best photo is kept. Without `ids`, every photo in the library (videos and virtual copies left out). Hashes come from each photo's grid thumbnail (the cached one where the grid has rendered it, else rendered now and cached) on a background job with a row in the activity stack (✕ stops it), on up to half the machine's cores at normal pace; a second run only renders photos not seen this session. When it ends, `filter` shows the groups alone (Clear Filters to go back) and `selectExtras` selects every photo that isn't its group's keeper, so Delete removes them; library.deleteDuplicates removes a group's copies and keeps their metadata. No catalog change and no undo step of its own. `wait: true` returns when the run is done (scripts); while a run goes, a call without `replace` only reports it, with `replace: true` it starts over with the new parameters, keeping the hashes made so far",
            super::always,
            find
        ),
        cmd!(
            query "duplicates.pump",
            "Find Duplicates in the Background",
            [],
            None,
            "{pace?: pause | light | normal} → {active, running?: {done, total}, last?, hashed} — cheap to call every frame (the app does): takes in what the background duplicate finder has hashed and, when it is done, shows the groups; hashes nothing new while paused and rests between photos when light",
            super::always,
            pump
        ),
        cmd!(
            "library.deleteDuplicates",
            "Delete Duplicate Photos",
            [],
            None,
            "{groups: [{keep: [id], delete: [id]}]} (or one group as {keep, delete}) → {deleted, kept, keywords, ratings, flags, labels} — what reviewed duplicate groups settle on: moves every `delete` photo to Recently Deleted (its file stays; Restore brings it back) after carrying what it has onto its group's `keep` photos — keywords they lack, a higher rating, a pick flag where the kept photo has none, a colour label where it has none — all as one undo step; the counts say how many kept photos gained each. Refuses a photo named twice, one not in the library, and a call that deletes nothing",
            super::always,
            delete_duplicates
        ),
    ]
}
