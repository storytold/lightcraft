//! Focus points for photos already in the library: `photo.readFocusPoints` (Photo ▸ Read Focus Points) and the
//! per-frame `focus.pump` that keeps the background job going. The job itself is `crate::focus_scan`.

use serde_json::Value;

use super::{CommandSpec, cmd};
use crate::{Result, Session};

#[cfg(not(target_arch = "wasm32"))]
fn read_points(s: &mut Session, p: &Value) -> Result<Value> {
    crate::focus_scan::read_points(s, p)
}

#[cfg(target_arch = "wasm32")]
fn read_points(_: &mut Session, _: &Value) -> Result<Value> {
    Err(super::bad("photo.readFocusPoints", "reading focus points needs the photos' files on disk"))
}

#[cfg(not(target_arch = "wasm32"))]
fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    crate::focus_scan::pump(s, p)
}

#[cfg(target_arch = "wasm32")]
fn pump(_: &mut Session, _: &Value) -> Result<Value> {
    Ok(serde_json::json!({"active": false}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "photo.readFocusPoints",
            "Read Focus Points",
            ["Photo"],
            None,
            "{ids?, wait?: false} → {queued, running, last?: {total, done, found, none, unavailable, bytesRead, seconds, cancelled}} — read the camera's focus point for photos imported before it was read (Sony α1 and A7S III raws and JPEGs), from the few KB of each file that hold it, on a background job with a row in the activity stack. Without `ids`, every photo in the library not looked at yet; with `ids`, those photos, looked at again. Photos that have a point keep it. No undo step and no sidecar write. `wait: true` returns when all are read (scripts)",
            super::always,
            read_points
        ),
        cmd!(
            query "focus.pump",
            "Read Focus Points in the Background",
            [],
            None,
            "{pace?: pause | light | normal} → {active, running?: {done, total, found}, last?, lookedAt} — cheap to call every frame (the app does): takes in what the background focus-point reader has read and commits it in batches; reads nothing new while paused and rests between files when light. Starts the reader by itself once per library opening when photos are left to look at",
            super::always,
            pump
        ),
    ]
}
