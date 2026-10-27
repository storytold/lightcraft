use super::*;

/// The selection after photos left the view (deleted, or rated, flagged or labelled out of it).
/// Its callers end an open edit (a slider drag) before their own change, as moving to the next
/// photo does, so it is its own undo step and comes first.
pub(super) fn select_after_deletion(s: &mut Session, before: &[PhotoId]) {
    let at = before.iter().position(|id| Some(*id) == s.selection.active);
    let remaining: std::collections::HashSet<_> = s.visible_cloned().into_iter().collect();
    s.selection.ids.retain(|id| remaining.contains(id));
    s.selection.active = s.selection.active.filter(|id| remaining.contains(id)).or_else(|| s.selection.ids.first().copied());
    if s.selection.active.is_none()
        && let Some(at) = at
    {
        // Keep culling near the deleted photo: next visible survivor, or the previous one at the end.
        let neighbour = before.iter().skip(at.saturating_add(1)).chain(before.iter().take(at).rev()).find(|id| remaining.contains(id));
        s.selection = neighbour.map(|id| Selection::single(*id)).unwrap_or_default();
    }
}

pub(super) fn album_param(p: &Value, key: &str, c: &str) -> Result<AlbumId> {
    p.get(key).and_then(Value::as_u64).map(AlbumId).ok_or_else(|| bad(c, format!("missing album `{key}`")))
}

/// An optional album id: absent or `null` is `None`, anything but a number is an error.
pub(super) fn opt_album_param(p: &Value, key: &str, c: &str) -> Result<Option<AlbumId>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(AlbumId(n))).ok_or_else(|| bad(c, format!("`{key}` must be an album id or null"))),
    }
}

pub(super) fn strs(p: &Value, key: &str) -> Vec<String> {
    p.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

/// The targets an album can hold and how many were browsed (Local) photos, which album views
/// never show (issue #684): those are skipped, and reported, instead of becoming invisible members.
pub(super) fn album_targets(s: &Session, targets: Vec<PhotoId>) -> (Vec<PhotoId>, usize) {
    let before = targets.len();
    let kept: Vec<PhotoId> = targets.into_iter().filter(|id| s.catalog.photo(*id).is_none_or(|p| !p.local)).collect();
    let skipped = before - kept.len();
    (kept, skipped)
}

/// Album targets, or the error for a selection that was nothing but Local photos.
pub(super) fn album_targets_or_err(s: &Session, command: &str, targets: Vec<PhotoId>) -> Result<(Vec<PhotoId>, usize)> {
    let (kept, skipped) = album_targets(s, targets);
    if kept.is_empty() && skipped > 0 {
        return Err(bad(command, crate::cmd::LOCAL_ONLY));
    }
    Ok((kept, skipped))
}

/// Move to Recently Deleted and drop the photos from stacks so leftover members do not keep
/// showing `i/n` (issue #1493).
pub(super) fn mark_deleted(s: &mut Session, ids: &[PhotoId], label: &str) -> Result<usize> {
    let mut ops: Vec<Op> = ids.iter().map(|id| Op::SetDeleted { id: *id, deleted: true }).collect();
    let n = ops.len();
    if n == 0 {
        return Ok(0);
    }
    ops.extend(s.catalog.remove_from_stacks_ops(ids));
    s.commit(label, Op::Batch { ops })?;
    Ok(n)
}

/// Apply one op per target as a single undo step.
pub(super) fn for_targets(s: &mut Session, p: &Value, label: &str, f: impl Fn(PhotoId) -> Option<Op>) -> Result<Value> {
    let targets = s.targets(p)?;
    let ops: Vec<Op> = targets.iter().filter_map(|id| f(*id)).collect();
    let n = ops.len();
    if n > 0 {
        s.commit(label, Op::Batch { ops })?;
    }
    Ok(json!({"changed": n}))
}

/// Adjust each target's own rating; unchanged bounds do not create an undo step.
pub(super) fn change_rating(s: &mut Session, p: &Value, delta: i8, label: &str) -> Result<Value> {
    let mut ops = Vec::new();
    for id in s.targets(p)? {
        let old = s.catalog.photo(id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?.rating;
        let rating = (i16::from(old) + i16::from(delta)).clamp(0, 5) as u8;
        if rating != old {
            ops.push(Op::SetRating { id, rating });
        }
    }
    let n = ops.len();
    if n > 0 {
        s.commit(label, Op::Batch { ops })?;
    }
    Ok(json!({"changed": n}))
}

/// Move each target's own flag by `step`: cycling wraps unflagged -> pick -> reject -> unflagged,
/// raising/lowering clamps on reject < unflagged < pick. Unchanged photos make no history step.
pub(super) fn change_flag(s: &mut Session, p: &Value, label: &str, step: impl Fn(Flag) -> Flag) -> Result<Value> {
    let mut ops = Vec::new();
    for id in s.targets(p)? {
        let old = s.catalog.photo(id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?.flag;
        let flag = step(old);
        if flag != old {
            ops.push(Op::SetFlag { id, flag });
        }
    }
    let n = ops.len();
    if n > 0 {
        s.commit(label, Op::Batch { ops })?;
    }
    Ok(json!({"changed": n}))
}

/// Rate, flag or label (`edit`), then `advance`. When the edit took the active photo out of the view (the filter bar,
/// Picks or a smart album shows it no more), culling goes on from its place, as after a delete: the photo after it is
/// active, or the one before it at the end (#2312), not the view's first photo. That photo is the next one, so
/// `advance` doesn't step past it. Photos named by `ids` / `id` leave the selection to the caller: Compare and Survey
/// keep showing the photo they rate.
pub(super) fn cull(s: &mut Session, p: &Value, edit: impl FnOnce(&mut Session) -> Result<Value>) -> Result<Value> {
    let named = [p.get("ids"), p.get("id")].into_iter().any(|t| t.is_some_and(|t| !t.is_null()));
    // the view as it was, then an open drag ends first, as its own undo step (the selection may
    // move on; the drag itself can take the photo out of the view)
    let before = (!named).then(|| s.visible_shared().1);
    if !named {
        s.end_interaction()?;
    }
    let active = s.selection.active;
    let v = edit(s)?;
    if let Some(before) = before
        && active.is_some_and(|a| before.contains(&a) && !s.visible().contains(&a))
    {
        select_after_deletion(s, &before);
        s.active_mask = None;
        s.active_spot = None;
    } else {
        advance_if(s, p);
    }
    Ok(v)
}

fn advance_if(s: &mut Session, p: &Value) {
    if bool_or(p, "advance", false) {
        let _ = step(s, 1);
    }
}

pub(super) fn step(s: &mut Session, d: isize) -> Result<Value> {
    let vis = s.visible_cloned();
    if vis.is_empty() {
        return ok();
    }
    let cur = s.selection.active.and_then(|a| vis.iter().position(|x| *x == a));
    let next = match cur {
        Some(i) => (i as isize + d).clamp(0, vis.len() as isize - 1) as usize,
        None => 0,
    };
    s.end_interaction()?;
    s.selection = Selection::single(vis[next]);
    s.active_mask = None;
    s.active_spot = None;
    Ok(json!({"active": vis[next].0}))
}

/// `library.up` / `library.down`: how many photos to move (at least 1, capped so hostile input
/// can't overflow).
pub(super) fn row_step(p: &Value) -> isize {
    p.get("rows").and_then(Value::as_u64).unwrap_or(1).clamp(1, 1 << 20) as isize
}

pub(super) fn rotate(s: &mut Session, p: &Value, cw: bool) -> Result<Value> {
    let targets = s.targets(p)?;
    let mut ops = Vec::new();
    for id in targets {
        if let Some(d) = s.develop_of(id) {
            let mut d = (*d).clone();
            d.orientation = d.orientation.rotated(cw);
            // the crop lives in the oriented frame, so it turns with the image (else it keeps the old width and height and cuts the photo)
            let turn = lightcraft_geom::Orientation::Normal.rotated(cw);
            d.crop.geometry.rect = turn.map_norm_rect(d.crop.geometry.rect);
            d.crop.aspect = d.crop.aspect.map(|(w, h)| (h, w));
            ops.extend(s.develop_op(id, d, "Rotate"));
        }
    }
    let n = ops.len();
    s.commit(if cw { "Rotate Right" } else { "Rotate Left" }, Op::Batch { ops })?;
    Ok(json!({"changed": n}))
}

pub(super) fn flip(s: &mut Session, p: &Value, horizontal: bool) -> Result<Value> {
    let targets = s.targets(p)?;
    let mut ops = Vec::new();
    for id in targets {
        if let Some(d) = s.develop_of(id) {
            let mut d = (*d).clone();
            if horizontal {
                d.crop.flip_h = !d.crop.flip_h;
            } else {
                d.crop.flip_v = !d.crop.flip_v;
            }
            ops.extend(s.develop_op(id, d, "Flip"));
        }
    }
    let n = ops.len();
    s.commit("Flip", Op::Batch { ops })?;
    Ok(json!({"changed": n}))
}

/// `library.selectBy`: select the photos in view that match every given criterion.
pub(super) fn select_by(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.selectBy";
    let flag: Option<lightcraft_catalog::Flag> = match p.get("flag") {
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|_| bad(C, "flag is pick, reject or none"))?),
        None => None,
    };
    let rating = p.get("rating").and_then(Value::as_u64).map(|r| r.min(5) as u8);
    let op = p.get("ratingOp").and_then(Value::as_str).unwrap_or("gte");
    let label: Option<Option<lightcraft_catalog::ColorLabel>> = match p.get("label").and_then(Value::as_str) {
        Some("none") => Some(None),
        Some(l) => Some(Some(serde_json::from_value(json!(l)).map_err(|_| bad(C, format!("unknown label `{l}`")))?)),
        None => None,
    };
    if flag.is_none() && rating.is_none() && label.is_none() {
        return Err(bad(C, "give flag, rating and/or label"));
    }
    let matches = |ph: &lightcraft_catalog::Photo| {
        flag.is_none_or(|f| ph.flag == f)
            && rating.is_none_or(|r| match op {
                "eq" => ph.rating == r,
                "lte" => ph.rating <= r,
                _ => ph.rating >= r,
            })
            && label.is_none_or(|l| ph.label == l)
    };
    let mut ids: Vec<PhotoId> = if bool_or(p, "add", false) { s.selection.ids.clone() } else { Vec::new() };
    for id in s.visible_cloned() {
        if !ids.contains(&id) && s.catalog.photo(id).is_some_and(|ph| matches(ph)) {
            ids.push(id);
        }
    }
    let active = s.selection.active.filter(|a| ids.contains(a)).or(ids.first().copied());
    let n = ids.len();
    s.selection = Selection { ids, active };
    Ok(json!({"selected": n}))
}

pub(super) fn has_library(s: &Session) -> std::result::Result<(), String> {
    if s.library.is_some() { Ok(()) } else { Err("no library is open (in-memory session)".into()) }
}
