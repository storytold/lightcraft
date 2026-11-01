//! Find Duplicate Photos: `library.findDuplicates` (Photo ▸ Find Duplicate Photos) and the per-frame
//! `duplicates.pump` that keeps the background job going. The job itself is `crate::dupes`.

use serde_json::Value;

use super::{CommandSpec, cmd};
use crate::{Result, Session};

#[cfg(not(target_arch = "wasm32"))]
fn find(s: &mut Session, p: &Value) -> Result<Value> {
    crate::dupes::find(s, p)
}

#[cfg(target_arch = "wasm32")]
fn find(_: &mut Session, _: &Value) -> Result<Value> {
    Err(super::bad("library.findDuplicates", "finding duplicates runs on a worker thread, which the browser build has no room for yet"))
}

#[cfg(not(target_arch = "wasm32"))]
fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    crate::dupes::pump(s, p)
}

#[cfg(target_arch = "wasm32")]
fn pump(_: &mut Session, _: &Value) -> Result<Value> {
    Ok(serde_json::json!({"active": false}))
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
            "{ids?, distance?: 0..16 (6), sameMoment?: bool (false), filter?: bool (true), selectExtras?: bool (true), wait?: bool (false)} → {queued, running, last?: {groups: [{exact, keep: id, delete: [id], maxDistance, photos: [{id, keep, why?}]}], photos, decoded, unreadable, duplicates, seconds, cancelled}} — photos that are the same picture (the same file twice, an export beside its raw, a resized or re-saved copy): byte-identical files, and files whose perceptual hashes differ by at most `distance` bits with aspect ratios within 3 %; a photo joins a group only when it is within `distance` of every member, so a burst can't chain into one group. `sameMoment` also requires the same capture time (to the second) when both photos have one: copies keep the original's, the next frame of a burst doesn't. Each group is ranked by quality — raw over rendered, lossless over lossy, more pixels, a larger file, then edits and rating — and its best photo is kept. Without `ids`, every photo in the library (videos and virtual copies left out). Hashes come from each photo's grid thumbnail (the cached one where the grid has rendered it, else rendered now and cached) on a background job with a row in the activity stack (✕ stops it), on up to half the machine's cores at normal pace; a second run only renders photos not seen this session. When it ends, `filter` shows the groups alone (Clear Filters to go back) and `selectExtras` selects every photo that isn't its group's keeper, so Delete removes them. No catalog change and no undo step of its own. `wait: true` returns when the run is done (scripts)",
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
    ]
}
