//! Library commands: view source, filter/sort, selection, ratings/flags/labels, rotate, delete,
//! metadata, albums, import.

use lcc_catalog::{Album, AlbumId, ColorLabel, CopyrightStatus, Filter, Flag, GroupBy, Op, PhotoId, Sort, SortKey};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, has_active, has_library_selection, has_selection, ok, str_param};
use crate::{LibrarySource, Result, Selection, Session};

/// The selection after photos left the view (deleted, or rated, flagged or labelled out of it).
/// Its callers end an open edit (a slider drag) before their own change, as moving to the next
/// photo does, so it is its own undo step and comes first.
fn select_after_deletion(s: &mut Session, before: &[PhotoId]) {
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

/// The auto-import folder's visible files with their sizes (the file-system half of
/// `library.autoImportScan`; the app lists on a worker thread and passes `listing`).
pub fn list_auto_import_folder(folder: &str) -> std::result::Result<Vec<(String, u64)>, String> {
    AutoImportScope { folder: folder.to_string(), subfolders: false, skip: None }.list()
}

/// What one Auto Import listing looks at: the watched folder, whether its subfolders count, and
/// a folder never entered (the library's own, so copies that arrive there aren't imported twice).
/// `Send`: the app lists on a worker thread.
#[derive(Clone, Debug)]
pub struct AutoImportScope {
    pub folder: String,
    pub subfolders: bool,
    pub skip: Option<std::path::PathBuf>,
}

impl AutoImportScope {
    /// The visible files with their sizes. Without subfolders: the folder's own files, whatever
    /// their type. With: also those of its subfolders that are a supported photo type, found by a
    /// bounded walk (depth and entry limits, hidden entries and the library folder skipped, a
    /// folder link followed at most once so loops end, an unreadable subfolder skipped).
    pub fn list(&self) -> std::result::Result<Vec<(String, u64)>, String> {
        if !self.subfolders {
            return list_top_level(&self.folder);
        }
        let root = std::path::Path::new(&self.folder);
        // the watched folder itself must be readable, as without subfolders
        std::fs::read_dir(root).map_err(|e| format!("{}: {e}", self.folder))?;
        let limits = crate::walk::Limits { max_depth: crate::import::AUTO_IMPORT_DEPTH, max_entries: crate::import::AUTO_IMPORT_ENTRIES };
        let keep = |f: &std::path::Path| f.parent() == Some(root) || crate::import::is_supported(f);
        let w = crate::walk::files_in(root, self.skip.as_deref(), limits, keep);
        if w.truncated {
            log::warn!(
                "auto import: stopped looking in {} after {} entries (limits: {} entries, {} folder levels)",
                self.folder,
                w.entries,
                limits.max_entries,
                limits.max_depth
            );
        }
        Ok(w.files
            .into_iter()
            .filter_map(|p| {
                // through a link: its target's type and size
                let m = std::fs::metadata(&p).ok()?;
                m.is_file().then(|| (p.to_string_lossy().to_string(), m.len()))
            })
            .collect())
    }
}

fn list_top_level(folder: &str) -> std::result::Result<Vec<(String, u64)>, String> {
    let rd = std::fs::read_dir(folder).map_err(|e| format!("{folder}: {e}"))?;
    Ok(rd
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            // through a link: its target's type and size
            let m = std::fs::metadata(&path).ok()?;
            // but a link's own hidden attribute, not its target's, as in a folder walk
            let hidden =
                if e.file_type().is_ok_and(|t| t.is_symlink()) { crate::walk::is_hidden_entry(&e) } else { crate::walk::is_hidden_with(&path, &m) };
            (m.is_file() && !hidden).then(|| (path.to_string_lossy().to_string(), m.len()))
        })
        .collect())
}

/// `library.import`'s params, checked (no file-system calls).
pub struct ImportRequest {
    pub paths: Vec<String>,
    pub opts: crate::import::ImportOptions,
    pub album: Option<u64>,
    pub album_name: Option<String>,
}

/// Parse and check `library.import`'s params (the app's import task runs the import itself, on a
/// worker thread, with the same options).
pub fn import_params(s: &Session, p: &Value) -> Result<ImportRequest> {
    let paths = strs(p, "paths");
    let mode = match str_param(p, "mode").unwrap_or("add") {
        "add" => crate::import::ImportMode::Add,
        "copy" => crate::import::ImportMode::Copy,
        "move" => crate::import::ImportMode::Move,
        other => return Err(bad("library.import", format!("unknown mode `{other}` (add|copy|move)"))),
    };
    let preset = match str_param(p, "preset").filter(|x| !x.is_empty()) {
        Some(id) => Some(s.presets.iter().find(|x| x.id == id).cloned().ok_or_else(|| bad("library.import", format!("unknown preset `{id}`")))?),
        None => None,
    };
    // `presets`: a stack, applied in order after `preset` (the Import tab's, OneSlips LR-IMP-WINDOW)
    let presets = strs(p, "presets")
        .iter()
        .filter(|x| !x.is_empty())
        .map(|id| s.presets.iter().find(|x| x.id == *id).cloned().ok_or_else(|| bad("library.import", format!("unknown preset `{id}`"))))
        .collect::<Result<Vec<_>>>()?;
    let album = p.get("album").and_then(Value::as_u64);
    if let Some(a) = album
        && s.catalog.album(AlbumId(a)).is_none_or(|al| al.folder || al.is_smart())
    {
        return Err(bad("library.import", "collection must be a regular collection"));
    }
    let organize = match str_param(p, "organize") {
        Some(o) => crate::import::Organize::parse(o).ok_or_else(|| {
            bad("library.import", format!("unknown organize `{o}` (date|month|flat, or a folder template like {{date:%Y}}/{{date:%Y%m%d}})"))
        })?,
        None => Default::default(),
    };
    if let crate::import::Organize::Template(t) = &organize
        && let Some(e) = crate::rename::folder_template_error(t)
    {
        return Err(bad("library.import", e));
    }
    let metadata_preset = str_param(p, "metadataPreset").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    if let Some(n) = &metadata_preset
        && !s.metadata_presets.iter().any(|m| m.name.eq_ignore_ascii_case(n))
    {
        return Err(bad("library.import", format!("unknown metadata preset `{n}`")));
    }
    let on_deleted = match str_param(p, "onDeleted") {
        Some(v) => {
            crate::import::OnDeleted::parse(v).ok_or_else(|| bad("library.import", format!("unknown onDeleted `{v}` (restore|skip|fresh)")))?
        }
        None => Default::default(),
    };
    let rename_start = match p.get("renameStart").and_then(Value::as_u64) {
        Some(n) => usize::try_from(n).map_err(|_| bad("library.import", "renameStart is too large for this platform"))?,
        None => 1,
    };
    let opts = crate::import::ImportOptions {
        on_deleted,
        mode,
        preset,
        presets,
        keywords: strs(p, "keywords"),
        destination: str_param(p, "destination").map(str::to_string),
        organize,
        rename: str_param(p, "rename").map(str::to_string),
        rename_start,
        metadata_preset,
        convert_dng: bool_or(p, "dng", false),
        camera_profile: crate::profile_bodies::ImportProfile::parse(p.get("cameraProfile")).map_err(|e| bad("library.import", e))?,
        local: bool_or(p, "local", false),
        preset_profile: bool_or(p, "presetProfile", false),
        recipe: super::recipe_param(p, "library.import")?,
        import_duplicates: bool_or(p, "importDuplicates", false),
    };
    let album_name = str_param(p, "albumName").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    Ok(ImportRequest { paths, opts, album, album_name })
}

/// Why a browsed photo can't be deleted (OneSlips LR-IMP-NODELETE).
pub const BROWSED_NO_DELETE: &str =
    "browsed photos are not in the library, so there is nothing to delete: their files are never touched (reject them, or forget the folder)";

/// `ids` without the browsed (Local, not imported) photos: deleting never touches those (a mixed
/// selection keeps going with the library's photos); refused when nothing else is left.
fn not_browsed(s: &Session, ids: Vec<PhotoId>, c: &str) -> Result<Vec<PhotoId>> {
    let n = ids.len();
    let kept: Vec<PhotoId> = ids.into_iter().filter(|id| !s.catalog.photo(*id).is_some_and(|p| p.local)).collect();
    if kept.is_empty() && n > 0 {
        return Err(bad(c, BROWSED_NO_DELETE));
    }
    Ok(kept)
}

/// After a batch of an import was committed: the album (a new one named `album_name` when there
/// is no `album` and something was imported) gets the photos. Returns the report as JSON (with
/// `album`).
pub fn import_batch_done(s: &mut Session, report: crate::import::ImportReport, mut album: Option<u64>, album_name: Option<&str>) -> Result<Value> {
    let mut report = serde_json::to_value(report).unwrap_or_default();
    // photos an import restored from Recently Deleted take part like new ones (album, selection)
    let imported: Vec<u64> =
        ["imported", "restored"].iter().flat_map(|k| report[*k].as_array().into_iter().flatten().filter_map(Value::as_u64)).collect();
    if album.is_none()
        && let Some(name) = album_name
        && !imported.is_empty()
    {
        let r = s.execute("album.create", &json!({"name": name}))?;
        album = r["id"].as_u64();
    }
    if let Some(a) = album
        && !imported.is_empty()
    {
        // (album.addPhotos wants a selection: the new photos are about to be it)
        s.selection = Selection::single(PhotoId(imported[0]));
        s.execute("album.addPhotos", &json!({"id": a, "ids": imported}))?;
        report["album"] = json!(a);
    }
    // the previews the library builds at import (library.previewSettings atImport)
    let ids: Vec<PhotoId> = imported.iter().map(|i| PhotoId(*i)).collect();
    // connected Immich servers are asked whether they have these photos already (`remote.pump`)
    #[cfg(not(target_arch = "wasm32"))]
    s.remote.link_queue.extend(ids.iter().copied());
    crate::cmd::previews::build_after_import(s, &ids);
    Ok(report)
}

fn album_param(p: &Value, key: &str, c: &str) -> Result<AlbumId> {
    p.get(key).and_then(Value::as_u64).map(AlbumId).ok_or_else(|| bad(c, format!("missing album `{key}`")))
}

/// An optional album id: absent or `null` is `None`, anything but a number is an error.
fn opt_album_param(p: &Value, key: &str, c: &str) -> Result<Option<AlbumId>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(AlbumId(n))).ok_or_else(|| bad(c, format!("`{key}` must be an album id or null"))),
    }
}

fn strs(p: &Value, key: &str) -> Vec<String> {
    p.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

/// The regular album the library shows, if any (the one Custom Order sorts).
fn custom_album(s: &Session) -> Option<AlbumId> {
    match s.source {
        crate::LibrarySource::Album(a) if s.catalog.has_custom_order(a) => Some(a),
        _ => None,
    }
}

/// The album's photos in the order the user sees them: its Custom Order, or — while the album is
/// shown in another sort — the shown photos first, in the grid's order, then the rest.
fn shown_order(s: &mut Session, a: AlbumId) -> Vec<PhotoId> {
    let saved = s.catalog.album_photo_order(a);
    if custom_album(s) != Some(a) || s.sort.key == SortKey::Custom {
        return saved;
    }
    let members: std::collections::HashSet<PhotoId> = saved.iter().copied().collect();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<PhotoId> = s.visible().iter().copied().filter(|x| members.contains(x) && seen.insert(*x)).collect();
    out.extend(saved.into_iter().filter(|x| seen.insert(*x)));
    out
}

/// `ids` for an ordering command: album members, repeats dropped; an id not in the album is an error.
fn order_ids(s: &Session, a: AlbumId, p: &Value, c: &str) -> Result<Vec<PhotoId>> {
    let Some(raw) = p.get("ids").and_then(Value::as_array) else { return Err(bad(c, "missing `ids` (an array of photo ids)")) };
    let al = s.catalog.album(a).ok_or_else(|| bad(c, "no such collection"))?;
    let members: std::collections::HashSet<PhotoId> = al.photos.iter().copied().collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for v in raw {
        let id = v.as_u64().map(PhotoId).ok_or_else(|| bad(c, "`ids` must be photo ids"))?;
        if !members.contains(&id) {
            return Err(bad(c, format!("photo {} is not in the collection", id.0)));
        }
        if seen.insert(id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Saves the album's Custom Order (one undo step) and sorts the view by it when it's shown.
fn set_custom_order(s: &mut Session, a: AlbumId, order: Vec<PhotoId>, label: &str) -> Result<Value> {
    if s.catalog.album(a).is_some_and(|al| al.photo_order != order) {
        s.commit(label, Op::SetAlbumPhotoOrder { id: a, order: order.clone() })?;
    }
    if custom_album(s) == Some(a) && s.sort.key != SortKey::Custom {
        s.sort = Sort { key: SortKey::Custom, ascending: true, ..s.sort };
    }
    Ok(json!({"order": order.iter().map(|x| x.0).collect::<Vec<_>>()}))
}

/// The targets an album can hold and how many were browsed (Local) photos, which album views
/// never show (issue #684): those are skipped, and reported, instead of becoming invisible members.
/// An id the catalog doesn't have (named outright, or kept selected after an undo took its photo
/// away) is dropped before anything changes, and isn't counted: `skipped` stays the Local count
/// the toasts name.
fn album_targets(s: &Session, targets: Vec<PhotoId>) -> (Vec<PhotoId>, usize) {
    let mut skipped = 0;
    let kept: Vec<PhotoId> = targets
        .into_iter()
        .filter(|id| match s.catalog.photo(*id) {
            None => false,
            Some(p) if p.local => {
                skipped += 1;
                false
            }
            Some(_) => true,
        })
        .collect();
    (kept, skipped)
}

/// Album targets, or the error for a selection that was nothing but Local photos.
fn album_targets_or_err(s: &Session, command: &str, targets: Vec<PhotoId>) -> Result<(Vec<PhotoId>, usize)> {
    let (kept, skipped) = album_targets(s, targets);
    if kept.is_empty() && skipped > 0 {
        return Err(bad(command, super::LOCAL_ONLY));
    }
    Ok((kept, skipped))
}

/// Move to Recently Deleted and drop the photos from stacks so leftover members do not keep
/// showing `i/n` (issue #1493).
fn mark_deleted(s: &mut Session, ids: &[PhotoId], label: &str) -> Result<usize> {
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
fn for_targets(s: &mut Session, p: &Value, label: &str, f: impl Fn(PhotoId) -> Option<Op>) -> Result<Value> {
    let targets = s.targets(p)?;
    apply_each(s, &targets, label, f)
}

/// Targets of rating, flag and label commands: [`Session::targets`], where a selected photo of a
/// collapsed stack stands for the whole stack (issue #2232). Explicit `ids` / `id` are kept as given.
fn mark_targets(s: &Session, p: &Value) -> Result<Vec<PhotoId>> {
    let targets = s.targets(p)?;
    let explicit = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    if explicit("ids") || explicit("id") {
        return Ok(targets);
    }
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<PhotoId> = Vec::with_capacity(targets.len());
    for id in targets {
        let members = s.catalog.stack_of(id).filter(|st| st.collapsed).map_or_else(|| vec![id], |st| st.photos.clone());
        out.extend(members.into_iter().filter(|m| seen.insert(*m)));
    }
    Ok(out)
}

/// [`for_targets`] on [`mark_targets`].
fn for_marks(s: &mut Session, p: &Value, label: &str, f: impl Fn(PhotoId) -> Option<Op>) -> Result<Value> {
    let targets = mark_targets(s, p)?;
    apply_each(s, &targets, label, f)
}

fn apply_each(s: &mut Session, targets: &[PhotoId], label: &str, f: impl Fn(PhotoId) -> Option<Op>) -> Result<Value> {
    let ops: Vec<Op> = targets.iter().filter_map(|id| f(*id)).collect();
    let n = ops.len();
    if n > 0 {
        s.commit(label, Op::Batch { ops })?;
    }
    Ok(json!({"changed": n}))
}

/// Adjust each target's own rating; unchanged bounds do not create an undo step.
fn change_rating(s: &mut Session, p: &Value, delta: i8, label: &str) -> Result<Value> {
    let mut ops = Vec::new();
    for id in mark_targets(s, p)? {
        let old = s.catalog.photo(id).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?.rating;
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
fn change_flag(s: &mut Session, p: &Value, label: &str, step: impl Fn(Flag) -> Flag) -> Result<Value> {
    let mut ops = Vec::new();
    for id in mark_targets(s, p)? {
        let old = s.catalog.photo(id).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?.flag;
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
fn cull(s: &mut Session, p: &Value, edit: impl FnOnce(&mut Session) -> Result<Value>) -> Result<Value> {
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

fn step(s: &mut Session, d: isize) -> Result<Value> {
    step_in(s, d, false)
}

/// [`step`], and with `within` (← / →) through the selection when several photos are selected (ported from OneSlips,
/// NOTES-1028 note 16): in the view's order, the selection kept, wrapping round; the photos between are skipped.
/// ↑ / ↓ and Auto Advance step through the view.
fn step_in(s: &mut Session, d: isize, within: bool) -> Result<Value> {
    let vis = s.visible_cloned();
    if vis.is_empty() {
        return ok();
    }
    let cur = s.selection.active.and_then(|a| vis.iter().position(|x| *x == a));
    if within
        && let Some(i) = cur
        && s.selection.ids.len() > 1
    {
        let picked: std::collections::HashSet<PhotoId> = s.selection.ids.iter().copied().collect();
        let order: Vec<PhotoId> = vis.iter().copied().filter(|x| picked.contains(x)).collect();
        if order.len() > 1
            && let Some(at) = vis.get(i).and_then(|a| order.iter().position(|x| x == a))
        {
            let n = order.len() as isize;
            let to = (at as isize + d.signum()).rem_euclid(n) as usize;
            let Some(&next) = order.get(to) else { return ok() };
            s.end_interaction()?;
            let (was, was_mask) = (s.selection.active, s.active_mask);
            s.selection.active = Some(next);
            s.active_mask = s.carried_mask(was, was_mask, s.selection.active);
            s.active_spot = None;
            return Ok(json!({"active": next.0}));
        }
    }
    let next = match cur {
        Some(i) => (i as isize + d).clamp(0, vis.len() as isize - 1) as usize,
        None => 0,
    };
    s.end_interaction()?;
    let (was, was_mask) = (s.selection.active, s.active_mask);
    s.selection = Selection::single(vis[next]);
    s.active_mask = s.carried_mask(was, was_mask, s.selection.active);
    s.active_spot = None;
    Ok(json!({"active": vis[next].0}))
}

/// `library.nextUnrated` / `library.prevUnrated` (ported from upstream #3904 in our own form): jump to the next
/// (`d > 0`) or previous photo of the view with no rating, wrapping round. With several photos selected the target is
/// added to the selection, which is kept. `{"allRated": true}` when nothing in the view is unrated.
fn step_unrated(s: &mut Session, d: isize) -> Result<Value> {
    let vis = s.visible_cloned();
    if vis.is_empty() {
        return ok();
    }
    let unrated: Vec<bool> = vis.iter().map(|id| s.catalog.photo(*id).is_some_and(|p| p.rating == 0)).collect();
    if !unrated.iter().any(|u| *u) {
        return Ok(json!({"allRated": true}));
    }
    let n = vis.len();
    let cur = s.selection.active.and_then(|a| vis.iter().position(|x| *x == a));
    // Walk away from the current photo (from the near end when there is none), wrapping once round; the current
    // photo is the last candidate, so the only unrated photo stays where it is.
    let target = (1..=n).find_map(|k| {
        let i = match (cur, d >= 0) {
            (Some(c), true) => c.checked_add(k)? % n,
            (Some(c), false) => c.checked_add(n.checked_sub(k % n)?)? % n,
            (None, true) => k.checked_sub(1)?,
            (None, false) => n.checked_sub(k)?,
        };
        unrated.get(i).copied().unwrap_or(false).then_some(i)
    });
    let Some(id) = target.and_then(|i| vis.get(i)).copied() else {
        return Ok(json!({"allRated": true}));
    };
    if s.selection.active == Some(id) {
        return Ok(json!({"active": id.0}));
    }
    s.end_interaction()?;
    let (was, was_mask) = (s.selection.active, s.active_mask);
    if s.selection.ids.len() > 1 {
        if !s.selection.contains(id) {
            s.selection.ids.push(id);
        }
        s.selection.active = Some(id);
    } else {
        s.selection = Selection::single(id);
    }
    s.active_mask = s.carried_mask(was, was_mask, s.selection.active);
    s.active_spot = None;
    Ok(json!({"active": id.0}))
}

/// `library.up` / `library.down`: how many photos to move (at least 1, capped so hostile input
/// can't overflow).
fn row_step(p: &Value) -> isize {
    p.get("rows").and_then(Value::as_u64).unwrap_or(1).clamp(1, 1 << 20) as isize
}

fn rotate(s: &mut Session, p: &Value, cw: bool) -> Result<Value> {
    let targets = s.targets(p)?;
    let mut ops = Vec::new();
    for id in targets {
        if let Some(before) = s.develop_of(id) {
            let mut d = (*before).clone();
            d.orientation = d.orientation.rotated(cw);
            // the crop lives in the oriented frame, so it turns with the image (else it keeps the old width and height and cuts the photo)
            let turn = lcc_geom::Orientation::Normal.rotated(cw);
            d.crop.geometry.rect = turn.map_norm_rect(d.crop.geometry.rect);
            // and so do the masks: on the photo first (an old one), then turned with it
            s.attach_masks(id, &before, &mut d);
            for m in &mut d.masks {
                lcc_pipeline::mask_space::turn(m, turn);
            }
            d.crop.aspect = d.crop.aspect.map(|(w, h)| (h, w));
            ops.extend(s.develop_op(id, d, "Rotate"));
        }
    }
    let n = ops.len();
    s.commit(if cw { "Rotate Right" } else { "Rotate Left" }, Op::Batch { ops })?;
    Ok(json!({"changed": n}))
}

fn flip(s: &mut Session, p: &Value, horizontal: bool) -> Result<Value> {
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

/// The active photo's metadata as `photo.copyMetadata` gives it (and `photo.setMeta` takes it).
fn active_meta(s: &Session, c: &str) -> Result<Value> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let m = &s.catalog.photo(id).ok_or_else(|| bad(c, "no photo"))?.meta;
    Ok(json!({"title": m.title, "headline": m.headline, "caption": m.caption, "descriptionWriter": m.description_writer,
        "category": m.category, "supplementalCategories": m.supplemental_categories, "subjectCode": m.subject_code,
        "altText": m.alt_text, "extendedDescription": m.extended_description,
        "copyright": m.copyright, "copyrightStatus": m.copyright_status.id(), "usageTerms": m.usage_terms, "copyrightUrl": m.copyright_url,
        "creator": m.creator, "location": m.location, "city": m.city, "state": m.state, "country": m.country, "countryCode": m.country_code,
        "keywords": m.keywords, "creatorJobTitle": m.contact.job_title, "creatorAddress": m.contact.address,
        "creatorCity": m.contact.city, "creatorState": m.contact.state, "creatorPostalCode": m.contact.postal_code,
        "creatorCountry": m.contact.country, "creatorPhone": m.contact.phone, "creatorEmail": m.contact.email,
        "creatorWebsite": m.contact.website}))
}

/// `library.selectBy`: select the photos in view that match every given criterion.
fn select_by(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.selectBy";
    let flag: Option<lcc_catalog::Flag> = match p.get("flag") {
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|_| bad(C, "flag is pick, reject or none"))?),
        None => None,
    };
    let rating = p.get("rating").and_then(Value::as_u64).map(|r| r.min(5) as u8);
    let op = p.get("ratingOp").and_then(Value::as_str).unwrap_or("gte");
    let label: Option<Option<lcc_catalog::ColorLabel>> = match p.get("label").and_then(Value::as_str) {
        Some("none") => Some(None),
        Some(l) => Some(Some(serde_json::from_value(json!(l)).map_err(|_| bad(C, format!("unknown label `{l}`")))?)),
        None => None,
    };
    if flag.is_none() && rating.is_none() && label.is_none() {
        return Err(bad(C, "give flag, rating and/or label"));
    }
    let matches = |ph: &lcc_catalog::Photo| {
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

/// Seeds stay exactly representable as a JSON double, so web and agent clients read back what they wrote.
const MAX_SEED: u64 = 1 << 53;

/// `seed` as an integer in `0..2^53`, or `default` when absent or null; anything else is an error.
fn seed_param(p: &Value, cmd: &str, default: u64) -> Result<u64> {
    match p.get("seed") {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_u64().filter(|n| *n < MAX_SEED).ok_or_else(|| bad(cmd, "seed must be an integer from 0 to 2^53 - 1")),
    }
}

/// The seed after `prev` at time `now`: a mix of `prev` and the clock text, so repeated reshuffles
/// within one clock tick still differ (barring a 2^-53 collision). Kept below 2^53 so JSON
/// clients that read numbers as doubles (web, MCP agents) get the same seed back.
fn next_seed(prev: u64, now: &str) -> u64 {
    let t = now.bytes().fold(0u64, |h, b| lcc_catalog::mix64(h ^ u64::from(b)));
    (lcc_catalog::mix64(prev.wrapping_add(1)) ^ t) & (MAX_SEED - 1)
}

/// `library.source`: show the source `p` names (the part of it that is not Go Back's bookkeeping).
fn show_source(s: &mut Session, p: &Value) -> Result<Value> {
    let kind = str_param(p, "kind").unwrap_or("all");
    let left = (s.source_key(), s.filter.clone());
    s.source = match kind {
        "all" => LibrarySource::All,
        "recentlyAdded" => LibrarySource::RecentlyAdded,
        "recentlyDeleted" => LibrarySource::RecentlyDeleted,
        "picks" => LibrarySource::Picks,
        "missing" => LibrarySource::Missing,
        "previousImport" => LibrarySource::PreviousImport,
        "quickCollection" => LibrarySource::QuickCollection,
        "previousExport" => LibrarySource::PreviousExport,
        "album" => {
            let a = album_param(p, "id", "library.source")?;
            if s.catalog.album(a).is_none_or(|a| a.folder) {
                return Err(bad("library.source", "no such collection"));
            }
            // one album at a time: `to_filter` makes the album shown replace the filter's, and the
            // filter bar has no album chip, so a left-over one would narrow the next source unseen
            s.filter.album = None;
            LibrarySource::Album(a)
        }
        "libraryFolder" => {
            let path = str_param(p, "path").filter(|d| !d.trim().is_empty() && !lcc_catalog::query::folder_key(d).is_empty());
            let path = path.ok_or_else(|| bad("library.source", "libraryFolder needs the `path` of a folder from library.folders"))?;
            if lcc_catalog::folders::is_startup_disk(path) {
                return Err(bad("library.source", "the startup disk's path covers every disk: choose a folder in it"));
            }
            let f = Filter { library_folder: Some(path.to_string()), ..Default::default() };
            let ids = s.catalog.query(&f, &Sort::default());
            if ids.is_empty() {
                return Err(bad("library.source", format!("{path}: no photo in the catalog was imported from it")));
            }
            if covers_other_disks(s, path, &ids) {
                return Err(bad("library.source", format!("{path} holds other disks as well: choose a disk or a folder on one")));
            }
            s.library_folder = Some(path.to_string());
            // one folder at a time: a folder filter left over would be ignored
            s.filter.library_folder = None;
            LibrarySource::LibraryFolder
        }
        other => return Err(bad("library.source", format!("unknown source `{other}`"))),
    };
    s.switch_source_filter(left.0, left.1);
    s.keep_active_in_view();
    Ok(json!({"count": s.visible().len()}))
}

fn can_source_back(s: &Session) -> std::result::Result<(), String> {
    if s.source_history.can_back() { Ok(()) } else { Err("no earlier source".into()) }
}

fn can_source_forward(s: &Session) -> std::result::Result<(), String> {
    if s.source_history.can_forward() { Ok(()) } else { Err("no later source".into()) }
}

/// Go Back (`dir` -1) / Go Forward (1): the nearest source in that direction that can still be shown.
fn walk_sources(s: &mut Session, dir: i32) -> Result<Value> {
    let c = if dir < 0 { "library.sourceBack" } else { "library.sourceForward" };
    for (i, params) in s.source_history.candidates(dir) {
        if let Ok(r) = show_source(s, &params) {
            s.source_history.arrived(i);
            return Ok(r);
        }
    }
    Err(bad(c, if dir < 0 { "no earlier source can be shown" } else { "no later source can be shown" }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        // ---- view source / filter / sort
        cmd!(
            "library.source",
            "Show Source",
            [],
            None,
            "{kind: all|recentlyAdded|previousImport|previousExport|quickCollection|album|recentlyDeleted|picks|missing|libraryFolder, id?: albumId, path?: a path from library.folders (for libraryFolder)}",
            always,
            |s, p| {
                let before = crate::view::SourceHistory::params_of(s);
                let r = show_source(s, p)?;
                let after = crate::view::SourceHistory::params_of(s);
                s.source_history.chose(before, after);
                Ok(r)
            }
        ),
        cmd!(
            "library.sourceBack",
            "Go Back",
            [],
            Some("Cmd+Alt+Left"),
            "{}: show the source shown before this one (library.source history, 50 deep); sources that are gone are skipped → {count}",
            can_source_back,
            |s, _| walk_sources(s, -1)
        ),
        cmd!(
            "library.sourceForward",
            "Go Forward",
            [],
            Some("Cmd+Alt+Right"),
            "{}: show the source Go Back left; choosing another source clears these; sources that are gone are skipped → {count}",
            can_source_forward,
            |s, _| walk_sources(s, 1)
        ),
        cmd!(
            "library.findPreviousProcess",
            "Find Previous Process Photos",
            ["Library"],
            None,
            "{}: filters the view to the photos that render with an older process version than this build's newest (the rule `processVersion < latest`; Clear Filters to go back) → {count, latest}",
            always,
            |s, _| {
                let latest = lcc_develop::ProcessVersion::LATEST.0;
                s.execute(
                    "library.filter",
                    &json!({"ruleSet": {"match": "all", "rules": [{"field": "processVersion", "op": "lt", "value": latest}]}}),
                )?;
                Ok(json!({"count": s.visible().len(), "latest": latest}))
            }
        ),
        cmd!(
            "library.refine",
            "Refine Photos…",
            ["Library"],
            Some("Cmd+Alt+R"),
            "{}: a culling pass over the photos in view: picks become unflagged, unflagged photos become rejected, rejected ones stay; one undo step → {changed, rejected, unflagged}",
            always,
            |s, _| {
                let ids = s.visible_cloned();
                let mut ops = Vec::new();
                let (mut rejected, mut unflagged) = (0, 0);
                for id in ids {
                    match s.catalog.photo(id).map(|p| p.flag) {
                        Some(Flag::Pick) => {
                            ops.push(Op::SetFlag { id, flag: Flag::None });
                            unflagged += 1;
                        }
                        Some(Flag::None) => {
                            ops.push(Op::SetFlag { id, flag: Flag::Reject });
                            rejected += 1;
                        }
                        _ => {}
                    }
                }
                let changed = ops.len();
                if changed > 0 {
                    s.commit("Refine Photos", Op::Batch { ops })?;
                    s.keep_active_in_view();
                }
                Ok(json!({"changed": changed, "rejected": rejected, "unflagged": unflagged}))
            }
        ),
        cmd!(
            "library.lockFilters",
            "Lock Filters",
            [],
            None,
            "{on?: bool (default: toggle)}: on (the default), the filter bar's filter stays when another folder or collection is chosen; off, choosing a source drops the filter (or restores that source's own, see library.rememberFilters) → {on}",
            always,
            |s, p| {
                s.lock_filters = bool_or(p, "on", !s.lock_filters);
                s.save_view();
                Ok(json!({"on": s.lock_filters}))
            }
        ),
        cmd!(
            "library.rememberFilters",
            "Remember Each Source's Filters Separately",
            [],
            None,
            "{on?: bool (default: toggle)}: with Lock Filters off, each source gets back the filter it had when it was left → {on}",
            always,
            |s, p| {
                s.remember_filters = bool_or(p, "on", !s.remember_filters);
                if !s.remember_filters {
                    s.source_filters.clear();
                }
                s.save_view();
                Ok(json!({"on": s.remember_filters}))
            }
        ),
        cmd!(
            "library.showSubfolderPhotos",
            "Show Photos in Subfolders",
            [],
            None,
            "{on?: bool (default: toggle)}: whether a folder chosen in Folders also shows the photos of the folders inside it → {on, count}",
            always,
            |s, p| {
                let on = bool_or(p, "on", !s.folder_subfolders);
                s.folder_subfolders = on;
                s.keep_active_in_view();
                s.save_view();
                Ok(json!({"on": on, "count": s.visible().len()}))
            }
        ),
        cmd!(
            "library.filter",
            "Filter",
            [],
            None,
            "partial Filter: {text?, semantic?, rating?, ratingOp?: atLeast|exactly|atMost, flag?: pick|reject|none|null, label?, kind?, copy?: true (virtual copies only) | false (master photos only) | null, merged?: hdr|panorama|hdrPanorama|focusStack|any, immich?: linked|notLinked|probable, vignettingReference?: reference|none (Vignetting Calibration references in use; a withdrawn one counts as none), edited?, date?, libraryFolder?: a path from library.folders, keyword?, person?, camera?, lens?, keywords?/people?/cameras?/lenses?: [any of these values], keywordOp?/personOp?/lensOp?: any|all (how each list combines: any = OR, all = AND), labels?: [any of these labels], faces?: count|min+|range}",
            always,
            |s, p| {
                let mut v = serde_json::to_value(&s.filter).unwrap_or_default();
                lcc_develop::presets::deep_merge(&mut v, p);
                let f: Filter = serde_json::from_value(v).map_err(|e| bad("library.filter", e.to_string()))?;
                let mut f = f;
                f.semantic = f.semantic.map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
                // a blank folder is no folder: no hidden "filters active" state
                f.library_folder = f.library_folder.filter(|d| !d.trim().is_empty());
                if f.library_folder.is_some() && s.source == LibrarySource::LibraryFolder {
                    return Err(bad("library.filter", "a folder is already shown (library.source): show another source first"));
                }
                // checked like a smart album's rules when this call sets them: an unknown field or a value
                // that can't match is an error, not an empty grid. Rules that stopped checking since
                // (their album was deleted) don't block other filter changes.
                if let Some(rules) = f.rule_set.as_mut() {
                    rules.upgrade();
                }
                let sets_rules = p.get("ruleSet").is_some_and(|r| !r.is_null());
                if let Some(problems) = f.rule_set.as_ref().filter(|_| sets_rules).map(|rs| rs.check(&s.catalog)).filter(|p| !p.is_empty()) {
                    return Err(bad("library.filter", problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")));
                }
                // an album this call names is one the library has (one named before and since
                // deleted is let go of, `Session::reconcile_view`)
                if p.get("album").is_some_and(|a| !a.is_null()) && f.album.is_some_and(|a| s.catalog.album(a).is_none()) {
                    return Err(bad("library.filter", "no such collection"));
                }
                let sets_semantic = p.get("semantic").is_some();
                let sets_only = p.get("only").is_some();
                s.filter = f;
                if sets_semantic {
                    if let Some(q) = s.filter.semantic.clone() {
                        if !sets_only {
                            s.apply_semantic_filter("library.filter", Some(&q), None, None)?;
                        }
                    } else if !sets_only {
                        s.filter.only.clear();
                        s.filter.only_none = false;
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            s.ml.last_query = None;
                        }
                    }
                }
                s.shelved_filter = None;
                Ok(json!({"count": s.visible().len()}))
            }
        ),
        cmd!(
            query "library.folders",
            "Catalog Folders",
            [],
            None,
            "{} → [{name, path, count, own, volume, selectable, label?, children}] the disks and the folders the library's photos were imported from, with photo counts (subfolders included in `count`); show one with library.source {kind: libraryFolder, path} (rows with selectable false only open)",
            always,
            |s, _| Ok(serde_json::to_value(s.catalog.folder_tree()).unwrap_or_default())
        ),
        cmd!(
            "library.removeFolder",
            "Remove Folder from Catalog",
            [],
            None,
            "{path, disk?: bool}: move the library photos imported from this folder (and the folders inside it) to Recently Deleted; one undo step, no file is touched. A whole disk or share (`/Volumes/nas`, `C:\\`, `\\\\srv\\share`) goes only with `disk: true`, the startup disk never → {removed}",
            always,
            |s, p| {
                const C: &str = "library.removeFolder";
                let path = str_param(p, "path").filter(|d| !d.trim().is_empty()).ok_or_else(|| bad(C, "missing `path`"))?;
                if lcc_catalog::query::folder_key(path).is_empty() {
                    return Err(bad(C, format!("{path}: not a folder")));
                }
                if lcc_catalog::folders::is_startup_disk(path) {
                    return Err(bad(C, "choose a folder, not the whole startup disk"));
                }
                if lcc_catalog::folders::is_disk_root(path) && !bool_or(p, "disk", false) {
                    return Err(bad(C, format!("{path} is a whole disk or share: pass `disk: true` to remove everything on it")));
                }
                let f = Filter { library_folder: Some(path.to_string()), ..Default::default() };
                let ids = s.catalog.query(&f, &Sort::default());
                if ids.is_empty() {
                    return Err(bad(C, format!("{path}: no photo in the catalog was imported from it")));
                }
                if covers_other_disks(s, path, &ids) && !bool_or(p, "disk", false) {
                    return Err(bad(C, format!("{path} holds whole disks: pass `disk: true` to remove everything on them")));
                }
                mark_deleted(s, &ids, "Remove Folder from Catalog")?;
                if s.filter.library_folder.as_deref().is_some_and(|c| lcc_catalog::query::folder_within(c, path)) {
                    s.filter.library_folder = None;
                }
                // a filter put aside by Enable Filters must not bring the removed folder back
                if let Some(f) = s.shelved_filter.as_mut()
                    && f.library_folder.as_deref().is_some_and(|c| lcc_catalog::query::folder_within(c, path))
                {
                    f.library_folder = None;
                }
                if s.shelved_filter.as_ref() == Some(&Filter::default()) {
                    s.shelved_filter = None;
                }
                // the folder being shown is gone: back to everything, as for a deleted album
                if s.library_folder.as_deref().is_some_and(|c| lcc_catalog::query::folder_within(c, path)) {
                    s.library_folder = None;
                    if s.source == LibrarySource::LibraryFolder {
                        s.source = LibrarySource::All;
                    }
                }
                let vis = s.visible_cloned();
                s.selection = vis.first().map(|f| Selection::single(*f)).unwrap_or_default();
                Ok(json!({"removed": ids.len()}))
            }
        ),
        cmd!(
            "folder.label",
            "Set Folder Color Label",
            [],
            None,
            "{path, label: red|yellow|green|blue|purple|none}: give a folder of the library (one its photos were imported from, see library.folders) a colour label, or take it off (also once its photos are gone); one undo step. The label follows the folder when it is renamed or moved with folder.rename / folder.move → {path, label}",
            always,
            |s, p| {
                const C: &str = "folder.label";
                let path = str_param(p, "path").filter(|d| !d.trim().is_empty()).ok_or_else(|| bad(C, "missing `path`"))?;
                let label = match str_param(p, "label").ok_or_else(|| bad(C, "missing `label` (red|yellow|green|blue|purple|none)"))? {
                    "none" => None,
                    x => Some(ColorLabel::parse(x).ok_or_else(|| bad(C, format!("unknown label {x:?}")))?),
                };
                // taking a label off always works, also once the folder's photos are gone
                let f = Filter { library_folder: Some(path.to_string()), ..Default::default() };
                if label.is_some() && s.catalog.query(&f, &Sort::default()).is_empty() {
                    return Err(bad(C, format!("{path}: no photo in the catalog was imported from it")));
                }
                if label.is_none() && s.catalog.folder_record(path).is_none() {
                    return Ok(json!({"path": path, "label": label}));
                }
                let op = s.catalog.folder_label_op(path, label);
                s.commit(if label.is_some() { "Set Folder Color Label" } else { "Remove Folder Color Label" }, op)?;
                Ok(json!({"path": path, "label": label}))
            }
        ),
        // the Advanced Search panel's structured query (issue #846): lowered into a Filter here, so the
        // grid shows its results as any other filtered view
        cmd!(
            "library.advancedSearch",
            "Advanced Search",
            ["Library"],
            None,
            "{query?: AdvancedSearch | null}: run the Advanced Search panel's structured query (global words; file/album/tags; item properties: dimensions, rating, color label, aspect; caption/title; photograph info: camera, lens, focal length, ISO, shutter speed, aperture; location; audio/video duration; face count and people) and show its results in the grid; null or an absent query clears it → {count}",
            always,
            |s, p| {
                match p.get("query") {
                    None | Some(Value::Null) => s.filter = Default::default(),
                    Some(q) => {
                        let query: lcc_catalog::advanced_search::AdvancedSearch =
                            serde_json::from_value(q.clone()).map_err(|e| bad("library.advancedSearch", e.to_string()))?;
                        let f = query.to_filter().map_err(|e| bad("library.advancedSearch", e))?;
                        if f.album.is_some_and(|a| s.catalog.album(a).is_none()) {
                            return Err(bad("library.advancedSearch", "no such collection"));
                        }
                        s.filter = f;
                    }
                }
                s.shelved_filter = None;
                Ok(json!({"count": s.visible().len()}))
            }
        ),
        cmd!("library.clearFilter", "Clear Filters", ["Library", "Filters"], None, "{}", always, |s, _| {
            s.filter = Default::default();
            s.shelved_filter = None;
            #[cfg(not(target_arch = "wasm32"))]
            {
                s.ml.last_query = None;
            }
            Ok(json!({"count": s.visible().len()}))
        }),
        // Classic: Library > Enable Filters (Cmd+L) switches the filter off and on again as it was
        cmd!(
            "library.toggleFilters",
            "Enable Filters",
            ["Library", "Filters"],
            Some("Cmd+L"),
            "{}: with a filter set, puts it aside and shows everything; with one put aside, brings it back as it was (stars, flag, text… together). Setting or clearing a filter while it is off forgets the one put aside → {enabled, count}",
            always,
            |s, _| {
                if s.filter != Filter::default() {
                    s.shelved_filter = Some(std::mem::take(&mut s.filter));
                } else if let Some(mut f) = s.shelved_filter.take() {
                    // one folder at a time: the folder shown now wins over the one the filter had
                    if s.source == LibrarySource::LibraryFolder {
                        f.library_folder = None;
                    }
                    s.filter = f;
                }
                Ok(json!({"enabled": s.shelved_filter.is_none(), "count": s.visible().len()}))
            }
        ),
        // View > Hide Rejected Photos: the filter bar's Picked + Unflagged on one key
        cmd!(
            "library.hideRejected",
            "Hide Rejected Photos",
            ["View"],
            None,
            "{hide?: bool (default: the opposite of now)}: the flag filter shows picks and unflagged photos, or, from there, no longer filters by flag; the rest of the filter stays. `hide` asks for the menu row's state: when the row is already in it, nothing changes (so `hide: false` leaves Picked only or Rejected only as it is). Like any filter change, it forgets a filter put aside → {hidden, count}",
            always,
            |s, p| {
                let hide = match p.get("hide") {
                    None | Some(Value::Null) => !s.rejects_hidden(),
                    Some(v) => v.as_bool().ok_or_else(|| bad("library.hideRejected", "hide must be true or false"))?,
                };
                // already in the state asked for: another flag filter (Picked only…) or a filter put aside stays as it is
                if hide == s.rejects_hidden() {
                    return Ok(json!({"hidden": hide, "count": s.visible().len()}));
                }
                s.filter.flag = None;
                s.filter.flags = if hide { vec![Flag::Pick, Flag::None] } else { Vec::new() };
                s.shelved_filter = None;
                Ok(json!({"hidden": hide, "count": s.visible().len()}))
            }
        ),
        cmd!(
            "library.sort",
            "Sort",
            ["View", "Sort"],
            None,
            "{key?: captureDate|importDate|editDate|fileName|rating|fileSize|random|custom (Custom Order: a regular album's order set by hand), ascending?: bool, group?: auto|none|day|month|year, seed?: u64 (the shuffle `random` gives)}",
            always,
            |s, p| {
                let key: SortKey = match p.get("key") {
                    Some(k) => serde_json::from_value(k.clone()).map_err(|e| bad("library.sort", e.to_string()))?,
                    None => s.sort.key,
                };
                let group = match str_param(p, "group") {
                    Some(g) => GroupBy::parse(g).ok_or_else(|| bad("library.sort", "group must be auto|none|day|month|year"))?,
                    None => s.sort.group,
                };
                let mut seed = seed_param(p, "library.sort", s.sort.seed)?;
                let explicit = p.get("seed").is_some_and(|v| !v.is_null());
                let mut ascending = bool_or(p, "ascending", s.sort.ascending);
                if key == SortKey::Custom && s.sort.key != SortKey::Custom {
                    let Some(a) = custom_album(s) else {
                        return Err(bad("library.sort", "Custom Order sorts a regular collection: open one first"));
                    };
                    // no order of its own yet: it starts from what is shown now
                    if s.catalog.album(a).is_some_and(|al| al.photo_order.is_empty()) {
                        let order = shown_order(s, a);
                        s.commit("Custom Order", Op::SetAlbumPhotoOrder { id: a, order })?;
                    }
                    ascending = bool_or(p, "ascending", true);
                }
                if key == SortKey::Random && s.sort.key != SortKey::Random && !explicit {
                    // switching to Random is a fresh shuffle, not whatever seed was left behind
                    seed = next_seed(seed, &(s.clock)());
                }
                s.sort = Sort { key, ascending, group, seed };
                ok()
            }
        ),
        cmd!(
            "library.refresh",
            "Refresh View",
            ["Library", "Filters"],
            None,
            "{}: show what matches now: a view that holds its photos (`library.holdView`) lets go of the ones that stopped matching a smart album's rules, Picks or the filter bar since it was opened, sorted or last refreshed",
            always,
            |s, _| {
                s.refresh_view();
                ok()
            }
        ),
        cmd!(
            "library.holdView",
            "Hold View Until Refreshed",
            ["Library", "Filters"],
            None,
            "{on?: bool (toggles when omitted)} → {hold}: when on, an edit that makes a photo stop matching the view (a keyword given in a smart album of photos without keywords, a flag taken off in Picks, a rating under the filter bar's) keeps it in the view until `library.refresh`, Reshuffle, or another source, filter or sort; photos that start matching still appear at once, and deleting a photo or taking it out of the album or folder shown still takes it out of the view. Changing a smart album's rules is another view too. Off (the default): the view follows every edit. Kept with the library's preferences; asking for the state it already has changes nothing",
            always,
            |s, p| {
                let on = match p.get("on") {
                    None | Some(Value::Null) => !s.hold_view,
                    Some(Value::Bool(on)) => *on,
                    Some(_) => return Err(bad("library.holdView", "on must be true or false")),
                };
                if on != s.hold_view {
                    let was = s.hold_view;
                    s.hold_view = on;
                    if let Err(e) = s.save_prefs() {
                        // not kept: not changed either, so asking again does what was asked
                        s.hold_view = was;
                        return Err(e);
                    }
                    s.refresh_view();
                }
                Ok(json!({"hold": s.hold_view}))
            }
        ),
        cmd!(
            "library.shuffle",
            "Reshuffle",
            ["View", "Sort"],
            None,
            "{seed?: 0..2^53-1}: sort at random; without `seed` a new shuffle each time",
            always,
            |s, p| {
                let seed = match seed_param(p, "library.shuffle", s.sort.seed)? {
                    given if p.get("seed").is_some_and(|v| !v.is_null()) => given,
                    // derived from the previous seed and the session clock: reproducible under a test
                    // clock, and no RNG needed
                    prev => next_seed(prev, &(s.clock)()),
                };
                s.sort = Sort { key: SortKey::Random, seed, ..s.sort };
                ok()
            }
        ),
        cmd!(
            query "library.groups",
            "Date Groups",
            [],
            None,
            "{by?: day|month|year (default: the sort's grouping; auto = day)} → [{key, label, start, count}] date headers of the grid",
            always,
            |s, p| {
                let by = match str_param(p, "by") {
                    Some(g) => GroupBy::parse(g).ok_or_else(|| bad("library.groups", "by must be auto|none|day|month|year"))?,
                    None => s.sort.group,
                };
                let vis = s.visible_cloned();
                Ok(serde_json::to_value(s.catalog.date_runs(&vis, s.sort.key, by)).unwrap_or_default())
            }
        ),
        // ---- selection
        cmd!(
            "library.select",
            "Select Photos",
            [],
            None,
            "{ids: [id], active?: id, mode?: replace|add|toggle|range|click}: every id must be a photo of the library. toggle (⌘/Ctrl-click) and range (Shift-click, from the active photo) keep the active photo; click (a plain click on a picture) on a photo of a selection of several makes it the active one and keeps the selection, elsewhere it selects that photo alone",
            always,
            |s, p| {
                let ids = super::photo_ids(p, "library.select")?.unwrap_or_default();
                let active = p.get("active").and_then(Value::as_u64).map(PhotoId);
                let mode = str_param(p, "mode").unwrap_or("replace");
                // An id that is not in the library is an error, and the selection stays as it was:
                // selecting it would otherwise report success and leave the session with no usable
                // active photo (#182).
                if let Some(id) = ids.iter().chain(active.as_ref()).find(|id| s.catalog.photo(**id).is_none()) {
                    return Err(bad("library.select", format!("no such photo {}", id.0)));
                }
                s.end_interaction()?;
                let (was, was_mask) = (s.selection.active, s.active_mask);
                match mode {
                    "replace" => {
                        // a repeated id is one photo: keep the first of each, in the order given
                        let mut seen = std::collections::HashSet::with_capacity(ids.len());
                        let ids: Vec<PhotoId> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
                        s.selection = Selection { active: active.or(ids.first().copied()), ids };
                    }
                    "add" => {
                        for id in ids {
                            if !s.selection.contains(id) {
                                s.selection.ids.push(id);
                            }
                            s.selection.active = Some(id);
                        }
                    }
                    "toggle" => ids.into_iter().for_each(|id| s.selection.toggle(id)),
                    "click" => {
                        if let Some(id) = ids.first() {
                            s.selection.click(*id);
                        }
                    }
                    "range" => {
                        let vis = s.visible_cloned();
                        if let Some(id) = ids.first() {
                            s.selection.extend_to(*id, &vis);
                        }
                    }
                    other => return Err(bad("library.select", format!("unknown mode `{other}`"))),
                }
                // the selected mask stays: on the same active photo (a ⇧-click range, a ⌘-click),
                // or its linked copy on the new one
                s.active_mask = s.carried_mask(was, was_mask, s.selection.active);
                if s.selection.active != was {
                    s.active_spot = None;
                }
                Ok(json!({"selected": s.selection.ids.len()}))
            }
        ),
        cmd!(
            "library.selectAll",
            "Select All",
            ["Edit"],
            Some("Cmd+A"),
            "{}: every photo of the view; photos that join the view later (an import still running) are selected too, until the selection or the view changes",
            always,
            |s, _| {
                let vis = s.visible_cloned();
                s.selection = Selection { active: s.selection.active.filter(|a| vis.contains(a)).or(vis.first().copied()), ids: vis };
                s.all_selected = s.visible_key.as_ref().map(|k| k.1.clone());
                Ok(json!({"selected": s.selection.ids.len()}))
            }
        ),
        cmd!(query "library.devices", "Cameras and Cards", [], None, "{} → [{name, path (its DCIM folder), root}]: mounted volumes with a DCIM folder", always, |_, _| {
            Ok(serde_json::to_value(crate::devices::devices_now()).unwrap_or_default())
        }),
        cmd!(query "photo.copyMetadata", "Copy Metadata", ["Metadata"], Some("Cmd+Alt+Shift+C"), "{}: title, caption, copyright (notice, status, usage terms, info URL), creator, location and keywords of the active photo", has_active, |s, _| {
            let v = active_meta(s, "photo.copyMetadata")?;
            s.meta_clipboard = Some(v.clone());
            Ok(v)
        }),
        cmd!(
            "photo.syncMetadata",
            "Sync Metadata",
            ["Metadata"],
            None,
            "{fields?: [title|caption|copyright|copyrightStatus|usageTerms|copyrightUrl|creator|location|keywords|…] (default: all)}: copy the active photo's metadata (as photo.copyMetadata) to the other selected photos, one undo step → as photo.setMeta",
            has_selection,
            |s, p| {
                let c = "photo.syncMetadata";
                let active = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let others: Vec<u64> = s.selection.ids.iter().filter(|x| **x != active).map(|x| x.0).collect();
                if others.is_empty() {
                    return Err(bad(c, "select the photos to sync to as well as the active one"));
                }
                let v = active_meta(s, c)?;
                let fields: Option<Vec<String>> =
                    p.get("fields").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect());
                let mut params = serde_json::Map::new();
                for (k, v) in v.as_object().into_iter().flatten() {
                    if fields.as_ref().is_none_or(|f| f.iter().any(|x| x == k)) {
                        params.insert(k.clone(), v.clone());
                    }
                }
                params.insert("ids".into(), json!(others));
                s.execute("photo.setMeta", &Value::Object(params))
            }
        ),
        cmd!(
            "photo.pasteMetadata",
            "Paste Metadata",
            ["Metadata"],
            Some("Cmd+Alt+Shift+V"),
            "{ids?, fields?: [title|caption|copyright|copyrightStatus|usageTerms|copyrightUrl|creator|location|keywords] (default: all copied)}",
            has_selection,
            |s, p| {
                let c = "photo.pasteMetadata";
                let clip = s.meta_clipboard.clone().ok_or_else(|| bad(c, "nothing copied (photo.copyMetadata)"))?;
                let fields: Option<Vec<String>> =
                    p.get("fields").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect());
                let mut params = serde_json::Map::new();
                for (k, v) in clip.as_object().into_iter().flatten() {
                    if fields.as_ref().is_none_or(|f| f.iter().any(|x| x == k)) {
                        params.insert(k.clone(), v.clone());
                    }
                }
                if let Some(ids) = p.get("ids") {
                    params.insert("ids".into(), ids.clone());
                }
                s.execute("photo.setMeta", &Value::Object(params))
            }
        ),
        cmd!(
            "library.selectBy",
            "Select by Flag, Rating or Label",
            [],
            None,
            "{flag?: pick|reject|none, rating?: 0..5, ratingOp?: gte|eq|lte (default gte), label?: red|…|none, add?: bool (extend the selection)}: among the photos in view",
            always,
            select_by
        ),
        cmd!("library.selectNone", "Deselect All", ["Edit"], Some("Cmd+Shift+A"), "{}", always, |s, _| {
            let a = s.selection.active;
            s.selection = Selection { ids: a.into_iter().collect(), active: a };
            ok()
        }),
        cmd!(
            "library.invertSelection",
            "Invert Selection",
            ["Edit"],
            None,
            "{}: selects the photos in view that aren't selected and deselects the others → {selected}",
            always,
            |s, _| {
                let chosen: std::collections::HashSet<PhotoId> = s.selection.ids.iter().copied().collect();
                let ids: Vec<PhotoId> = s.visible_cloned().into_iter().filter(|id| !chosen.contains(id)).collect();
                // the active photo stays active if it is still selected; else the first of the new selection
                let active = s.selection.active.filter(|a| ids.contains(a)).or(ids.first().copied());
                let n = ids.len();
                s.selection = Selection { ids, active };
                s.all_selected = None;
                Ok(json!({"selected": n}))
            }
        ),
        cmd!(
            "library.deselectActive",
            "Deselect Active Photo",
            ["Edit"],
            None,
            "{}: takes the active photo out of the selection; the next selected photo (wrapping round) becomes active, and with none left nothing stays selected → {selected}",
            has_active,
            |s, _| {
                let Some(active) = s.selection.active else { return Err(bad("library.deselectActive", "no active photo")) };
                let ids = s.selection.ids.clone();
                let Some(at) = ids.iter().position(|i| *i == active) else {
                    // the active photo isn't in the selection: nothing to take out
                    return Ok(json!({"selected": ids.len()}));
                };
                let mut rest = ids;
                rest.remove(at);
                // "next" is the next selected photo after the old one, wrapping round
                let next = rest.get(at).or_else(|| rest.first()).copied();
                let n = rest.len();
                s.selection = Selection { ids: rest, active: next };
                s.all_selected = None;
                Ok(json!({"selected": n}))
            }
        ),
        cmd!(
            "library.deselectUnflagged",
            "Deselect Unflagged Photos",
            ["Edit"],
            None,
            "{}: keeps only the flagged (picked) photos of the selection selected; with none flagged it acts as Deselect All (the active photo stays) → {selected}",
            always,
            |s, _| {
                let keep: Vec<PhotoId> =
                    s.selection.ids.iter().copied().filter(|id| s.catalog.photo(*id).is_some_and(|ph| ph.flag == Flag::Pick)).collect();
                if keep.is_empty() {
                    let a = s.selection.active;
                    s.selection = Selection { ids: a.into_iter().collect(), active: a };
                    s.all_selected = None;
                    return Ok(json!({"selected": s.selection.ids.len()}));
                }
                let active = s.selection.active.filter(|a| keep.contains(a)).or(keep.first().copied());
                let n = keep.len();
                s.selection = Selection { ids: keep, active };
                s.all_selected = None;
                Ok(json!({"selected": n}))
            }
        ),
        cmd!(
            "library.next",
            "Next Photo",
            [],
            Some("Right"),
            "{}: with several photos selected: the next selected one (wrapping round), the selection kept",
            always,
            |s, _| step_in(s, 1, true)
        ),
        cmd!(
            "library.previous",
            "Previous Photo",
            [],
            Some("Left"),
            "{}: with several photos selected: the previous selected one (wrapping round), the selection kept",
            always,
            |s, _| step_in(s, -1, true)
        ),
        cmd!(
            "library.nextUnrated",
            "Next Unrated Photo",
            [],
            None,
            "{}: the next photo of the view with no rating, wrapping round; {allRated: true} when there is none",
            always,
            |s, _| step_unrated(s, 1)
        ),
        cmd!(
            "library.prevUnrated",
            "Previous Unrated Photo",
            [],
            None,
            "{}: the previous photo of the view with no rating, wrapping round; {allRated: true} when there is none",
            always,
            |s, _| step_unrated(s, -1)
        ),
        cmd!(
            "library.down",
            "Photo Below",
            [],
            Some("Down"),
            "{rows?: n (photos to move forward; default 1, the grid passes the distance to the cell below)}",
            always,
            |s, p| { step(s, row_step(p)) }
        ),
        cmd!(
            "library.up",
            "Photo Above",
            [],
            Some("Up"),
            "{rows?: n (photos to move back; default 1, the grid passes the distance to the cell above)}",
            always,
            |s, p| { step(s, -row_step(p)) }
        ),
        // ---- rating / flags / labels
        cmd!(
            "photo.rate",
            "Set Rating",
            ["Photo", "Set Rating"],
            None,
            "{rating: 0..5, ids?, advance?: bool}: without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| {
                let r = super::index_req(p, "rating", "photo.rate")
                    .ok()
                    .filter(|r| *r <= 5)
                    .ok_or_else(|| bad("photo.rate", "rating must be a whole number 0..5"))?;
                cull(s, p, |s| for_marks(s, p, "Set Rating", |id| Some(Op::SetRating { id, rating: r as u8 })))
            }
        ),
        cmd!(
            "photo.decreaseRating",
            "Decrease Rating",
            [],
            None,
            "{ids?, advance?: bool}: without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| { cull(s, p, |s| change_rating(s, p, -1, "Decrease Rating")) }
        ),
        cmd!(
            "photo.increaseRating",
            "Increase Rating",
            [],
            None,
            "{ids?, advance?: bool}: without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| { cull(s, p, |s| change_rating(s, p, 1, "Increase Rating")) }
        ),
        cmd!(
            "photo.flag",
            "Set Flag",
            ["Photo", "Set Flag"],
            None,
            "{flag: pick|reject|none, ids?, advance?: bool}: without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| {
                let f = str_param(p, "flag").and_then(Flag::parse).ok_or_else(|| bad("photo.flag", "flag must be pick|reject|none"))?;
                cull(s, p, |s| for_marks(s, p, "Set Flag", |id| Some(Op::SetFlag { id, flag: f })))
            }
        ),
        // Classic: ` cycles the flag; Cmd+Up / Cmd+Down raise / lower it (Ctrl on Windows and Linux)
        cmd!(
            "photo.cycleFlag",
            "Cycle Flag",
            ["Photo", "Set Flag"],
            Some("`"),
            "{ids?, advance?: bool}: each photo's own flag: unflagged -> pick -> reject -> unflagged; without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| {
                cull(s, p, |s| {
                    change_flag(s, p, "Cycle Flag", |f| match f {
                        Flag::None => Flag::Pick,
                        Flag::Pick => Flag::Reject,
                        Flag::Reject => Flag::None,
                    })
                })
            }
        ),
        cmd!(
            "photo.increaseFlag",
            "Increase Flag Status",
            ["Photo", "Set Flag"],
            Some("Cmd+Up"),
            "{ids?, advance?: bool}: reject -> unflagged -> pick (stops at pick); without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| { cull(s, p, |s| change_flag(s, p, "Increase Flag Status", |f| if f == Flag::Reject { Flag::None } else { Flag::Pick })) }
        ),
        cmd!(
            "photo.decreaseFlag",
            "Decrease Flag Status",
            ["Photo", "Set Flag"],
            Some("Cmd+Down"),
            "{ids?, advance?: bool}: pick -> unflagged -> reject (stops at reject); without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| { cull(s, p, |s| change_flag(s, p, "Decrease Flag Status", |f| if f == Flag::Pick { Flag::None } else { Flag::Reject })) }
        ),
        cmd!(
            "photo.pick",
            "Flag as Pick",
            ["Photo", "Set Flag"],
            Some("P"),
            "{ids?, advance?: bool}: without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| { cull(s, p, |s| for_marks(s, p, "Flag as Pick", |id| Some(Op::SetFlag { id, flag: Flag::Pick }))) }
        ),
        cmd!(
            "photo.reject",
            "Flag as Reject",
            ["Photo", "Set Flag"],
            Some("X"),
            "{ids?, advance?: bool}: without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| { cull(s, p, |s| for_marks(s, p, "Flag as Reject", |id| Some(Op::SetFlag { id, flag: Flag::Reject }))) }
        ),
        cmd!(
            "photo.unflag",
            "Unflag",
            ["Photo", "Set Flag"],
            Some("U"),
            "{ids?, advance?: bool}: without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| { cull(s, p, |s| for_marks(s, p, "Unflag", |id| Some(Op::SetFlag { id, flag: Flag::None }))) }
        ),
        cmd!(
            "photo.label",
            "Set Color Label",
            ["Photo", "Set Color Label"],
            None,
            "{label: red|yellow|green|blue|purple|none, ids?, toggle?: bool, advance?: bool}: without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| {
                // `none` (or null) clears; a missing label or one that isn't text is an error, not a clear
                let mut l = match p.get("label") {
                    Some(Value::Null) => None,
                    Some(Value::String(x)) if x == "none" => None,
                    Some(Value::String(x)) => Some(ColorLabel::parse(x).ok_or_else(|| bad("photo.label", "unknown label"))?),
                    None => return Err(bad("photo.label", "missing `label` (red|yellow|green|blue|purple|none)")),
                    Some(_) => return Err(bad("photo.label", "label must be red|yellow|green|blue|purple|none")),
                };
                cull(s, p, |s| {
                    let targets = mark_targets(s, p)?;
                    if bool_or(p, "toggle", false) && l.is_some() {
                        let all_have = !targets.is_empty() && targets.iter().all(|id| s.catalog.photo(*id).and_then(|ph| ph.label) == l);
                        if all_have {
                            l = None;
                        }
                    }
                    let ops: Vec<Op> = targets.into_iter().map(|id| Op::SetLabel { id, label: l }).collect();
                    let n = ops.len();
                    if n > 0 {
                        s.commit("Set Color Label", Op::Batch { ops })?;
                    }
                    let label_str = match l {
                        Some(label) => format!("{label:?}").to_lowercase(),
                        None => "none".to_string(),
                    };
                    Ok(json!({"changed": n, "label": label_str}))
                })
            }
        ),
        // `photo.label` takes the colour as a parameter, so a key can't be given to "none" alone
        cmd!(
            "photo.clearLabel",
            "Clear Color Label",
            ["Photo", "Set Color Label"],
            None,
            "{ids?, advance?: bool}: `photo.label` with `none`; without `ids`, the selection moves on when this takes the active photo out of the view",
            has_selection,
            |s, p| { cull(s, p, |s| for_targets(s, p, "Set Color Label", |id| Some(Op::SetLabel { id, label: None }))) }
        ),
        // ---- orientation
        cmd!("photo.rotateLeft", "Rotate Left", ["Photo", "Rotate & Flip"], Some("Cmd+["), "{ids?}", has_selection, |s, p| rotate(s, p, false)),
        cmd!("photo.rotateRight", "Rotate Right", ["Photo", "Rotate & Flip"], Some("Cmd+]"), "{ids?}", has_selection, |s, p| rotate(s, p, true)),
        cmd!("photo.flipHorizontal", "Flip Horizontal", ["Photo", "Rotate & Flip"], None, "{ids?}", has_selection, |s, p| flip(s, p, true)),
        cmd!("photo.flipVertical", "Flip Vertical", ["Photo", "Rotate & Flip"], None, "{ids?}", has_selection, |s, p| flip(s, p, false)),
        // ---- face / pet regions
        cmd!(
            "photo.removeRegion",
            "Remove Face Box",
            [],
            None,
            "{id?, index}: remove one face / pet region (by its position in the photo's regions) from the photo in the catalog; undoable. The XMP sidecar is never rewritten for this, even with auto-write on, so reading the metadata from the file brings the region back",
            always,
            |s, p| {
                let id = s.photo_or_active(p, "photo.removeRegion")?;
                let index = p
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|i| usize::try_from(i).ok())
                    .ok_or_else(|| bad("photo.removeRegion", "missing or invalid `index`"))?;
                let mut meta = s.catalog.photo(id).ok_or_else(|| bad("photo.removeRegion", "no such photo"))?.meta.clone();
                if index >= meta.regions.len() {
                    return Err(bad("photo.removeRegion", "no such region"));
                }
                let gone = meta.regions.remove(index);
                s.commit("Remove Face Box", Op::SetMeta { id, meta: Box::new(meta) })?;
                s.skip_auto_write = true;
                Ok(json!({"removed": gone.name}))
            }
        ),
        cmd!(
            "photo.setRegion",
            "Resize Face Box",
            [],
            None,
            "{id?, index, rect: {x0, y0, x1, y1}}: set one face / pet region's box (normalized, in the photo's upright frame; clamped to the photo, at least 0.5 % each way) in the catalog; undoable. Like photo.removeRegion it never rewrites the XMP sidecar",
            always,
            |s, p| {
                const C: &str = "photo.setRegion";
                // a drag is one undo step however many frames it took: the caller commits once, on release
                let id = s.photo_or_active(p, C)?;
                let index = p
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|i| usize::try_from(i).ok())
                    .ok_or_else(|| bad(C, "missing or invalid `index`"))?;
                let r = p.get("rect").ok_or_else(|| bad(C, "missing `rect`"))?;
                let num = |k: &str| {
                    r.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| bad(C, format!("`rect.{k}` must be a finite number")))
                };
                let (a, b, c, d) = (num("x0")?, num("y0")?, num("x1")?, num("y1")?);
                let (x0, x1) = (a.min(c).clamp(0.0, 1.0), a.max(c).clamp(0.0, 1.0));
                let (y0, y1) = (b.min(d).clamp(0.0, 1.0), b.max(d).clamp(0.0, 1.0));
                if x1 - x0 < 0.005 || y1 - y0 < 0.005 {
                    return Err(bad(C, "the box would be too small"));
                }
                let mut meta = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?.meta.clone();
                let region = meta.regions.get_mut(index).ok_or_else(|| bad(C, "no such region"))?;
                region.rect = lcc_geom::Rect { x0, y0, x1, y1 };
                s.commit("Resize Face Box", Op::SetMeta { id, meta: Box::new(meta) })?;
                s.skip_auto_write = true;
                Ok(json!({"rect": {"x0": x0, "y0": y0, "x1": x1, "y1": y1}}))
            }
        ),
        cmd!(
            "photo.addRegion",
            "Add Face Region",
            [],
            None,
            "{id?, rect: {x0, y0, x1, y1}, name?, kind?: \"face\"|\"pet\"} → {index, rect}: add a hand-drawn face (or pet) region, the same kind the detector writes, to the photo in the catalog (box normalized in the photo's upright frame, inside 0..1, at least 0.5 % each way; `name` makes it a named face); undoable. Like photo.removeRegion it never rewrites the XMP sidecar",
            always,
            |s, p| {
                const C: &str = "photo.addRegion";
                let id = s.photo_or_active(p, C)?;
                let r = p.get("rect").ok_or_else(|| bad(C, "missing `rect`"))?;
                let num = |k: &str| {
                    r.get(k)
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                        .ok_or_else(|| bad(C, format!("`rect.{k}` must be a number from 0 to 1")))
                };
                let (a, b, c, d) = (num("x0")?, num("y0")?, num("x1")?, num("y1")?);
                let (x0, x1) = (a.min(c), a.max(c));
                let (y0, y1) = (b.min(d), b.max(d));
                if x1 - x0 < 0.005 || y1 - y0 < 0.005 {
                    return Err(bad(C, "the box is too small"));
                }
                let kind = match p.get("kind").and_then(Value::as_str) {
                    None | Some("face") => lcc_meta::RegionKind::Face,
                    Some("pet") => lcc_meta::RegionKind::Pet,
                    Some(_) => return Err(bad(C, "`kind` must be face or pet")),
                };
                let name = match p.get("name") {
                    None | Some(Value::Null) => None,
                    Some(v) => {
                        let t = v.as_str().ok_or_else(|| bad(C, "`name` must be text"))?.trim();
                        (!t.is_empty()).then(|| t.to_string())
                    }
                };
                let mut meta = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?.meta.clone();
                let index = meta.regions.len();
                meta.regions.push(lcc_meta::Region { rect: lcc_geom::Rect { x0, y0, x1, y1 }, kind, name, description: None });
                s.commit("Add Face Region", Op::SetMeta { id, meta: Box::new(meta) })?;
                s.skip_auto_write = true;
                Ok(json!({"index": index, "rect": {"x0": x0, "y0": y0, "x1": x1, "y1": y1}}))
            }
        ),
        // ---- delete / restore
        cmd!(
            "photo.delete",
            "Delete Photo",
            ["Photo"],
            Some("Delete"),
            "{ids?}: moves to Recently Deleted (browsed photos not in the library are left alone: reject them instead; a selection of nothing else is refused)",
            has_selection,
            |s, p| {
                let before = s.visible_cloned();
                let targets = not_browsed(s, s.targets(p)?, "photo.delete")?;
                // an open drag ends first, as its own undo step (the selection moves on)
                s.end_interaction()?;
                let n = mark_deleted(s, &targets, "Delete")?;
                select_after_deletion(s, &before);
                Ok(json!({"changed": n}))
            }
        ),
        cmd!("photo.restore", "Restore", ["Photo"], None, "{ids?}", has_selection, |s, p| for_targets(s, p, "Restore", |id| Some(Op::SetDeleted {
            id,
            deleted: false
        }))),
        cmd!(
            "library.emptyRecentlyDeleted",
            "Empty Recently Deleted",
            ["Photo"],
            None,
            "{} → {deleted}: removes every photo in Recently Deleted from the library (files on disk stay)",
            always,
            |s, _| {
                let ids: Vec<PhotoId> = s.catalog.photos().filter(|p| p.deleted).map(|p| p.id).collect();
                if ids.is_empty() {
                    return Ok(json!({"deleted": 0}));
                }
                let op = s.catalog.delete_photos_permanently_ops(&ids);
                s.commit("Empty Recently Deleted", op)?;
                s.selection = Selection::default();
                Ok(json!({"deleted": ids.len()}))
            }
        ),
        cmd!(
            "photo.deletePermanently",
            "Delete Permanently",
            ["Photo"],
            None,
            "{ids?} (browsed photos not in the library are left alone)",
            has_selection,
            |s, p| {
                let before = s.visible_cloned();
                let t = not_browsed(s, s.targets(p)?, "photo.deletePermanently")?;
                // before the inverse ops are taken, so undo brings the photo back with its edit
                s.end_interaction()?;
                let op = s.catalog.delete_photos_permanently_ops(&t);
                s.commit("Delete Permanently", op)?;
                select_after_deletion(s, &before);
                Ok(json!({"deleted": t.len()}))
            }
        ),
        cmd!(
            "photo.removeFromCatalog",
            "Remove Photo from Catalog",
            ["Photo"],
            None,
            "{ids?}: removes the photos from the library without asking; the files stay on disk. One undo step (browsed photos not in the library are left alone) → {deleted}",
            has_selection,
            |s, p| {
                let before = s.visible_cloned();
                let t = not_browsed(s, s.targets(p)?, "photo.removeFromCatalog")?;
                s.end_interaction()?;
                let op = s.catalog.delete_photos_permanently_ops(&t);
                s.commit("Remove from Catalog", op)?;
                select_after_deletion(s, &before);
                Ok(json!({"deleted": t.len()}))
            }
        ),
        cmd!(
            "photo.deleteFromDisk",
            "Delete from Disk",
            ["Photo", "File"],
            None,
            "{ids?}: moves the photos' files to the system trash and removes them from the library (refused for browsed photos not in the library: their files are never touched)",
            has_selection,
            |s, p| {
                let before = s.visible_cloned();
                if s.targets(p)?.iter().any(|id| s.catalog.photo(*id).is_some_and(|ph| ph.local)) {
                    return Err(bad("photo.deleteFromDisk", BROWSED_NO_DELETE));
                }
                let r = crate::disk_delete::delete_from_disk(s, p)?;
                select_after_deletion(s, &before);
                Ok(r)
            }
        ),
        cmd!(query "library.tally", "Culling Counts", [], None, "{} → {total, picks, rejects, unflagged, stars: [0★..5★], labels: [red, yellow, green, blue, purple], unlabeled}: the view's photos by culling mark, counted without the stars / flags / labels filter itself", always, |s, _| {
            Ok(serde_json::to_value(s.tally()).unwrap_or_default())
        }),
        cmd!(
            "library.deleteRejected",
            "Delete Rejected Photos",
            ["Photo", "File"],
            None,
            "{files?: keep (default) | recycle, confirm?: bool}: every rejected photo of the view in one go: keep removes them from the library to Recently Deleted (one undo step, the files stay on disk); recycle moves their files (and the sidecars that are theirs alone) to the system trash as Delete from Disk does, and needs confirm: true → {deleted, files}",
            always,
            |s, p| {
                let files = str_param(p, "files").unwrap_or("keep");
                if !matches!(files, "keep" | "recycle") {
                    return Err(bad("library.deleteRejected", format!("unknown files `{files}` (keep|recycle)")));
                }
                let rejected: Vec<u64> = s
                    .visible_cloned()
                    .into_iter()
                    // (browsed photos not in the library are never deleted: LR-IMP-NODELETE)
                    .filter(|id| s.catalog.photo(*id).is_some_and(|ph| ph.flag == Flag::Reject && !ph.local))
                    .map(|id| id.0)
                    .collect();
                if rejected.is_empty() {
                    return Ok(json!({"deleted": 0, "files": files}));
                }
                let n = rejected.len();
                if files == "recycle" {
                    let confirm = bool_or(p, "confirm", false);
                    s.execute("photo.deleteFromDisk", &json!({"ids": rejected, "confirm": confirm}))?;
                } else {
                    s.execute("photo.delete", &json!({"ids": rejected}))?;
                }
                Ok(json!({"deleted": n, "files": files}))
            }
        ),
        // ---- metadata
        cmd!(
            "photo.setMeta",
            "Edit Info",
            [],
            None,
            "{ids?, title?, headline?, caption?, descriptionWriter?, category?, supplementalCategories?: [..] | \"a, b\", subjectCode?: [..] | \"c, d\", altText?, extendedDescription?, copyright?, copyrightStatus?: unknown|copyrighted|publicDomain, usageTerms?, copyrightUrl?, creator?, creatorJobTitle?, creatorAddress?, creatorCity?, creatorState?, creatorPostalCode?, creatorCountry?, creatorPhone?, creatorEmail?, creatorWebsite?, location?, city?, state?, country?, countryCode?, gps?: \"lat, lon\" | [lat, lon] | null, keywords?: [..], addKeywords?: [..], removeKeywords?: [..]}",
            has_selection,
            |s, p| {
                let strs = |k: &str| {
                    p.get(k).and_then(|v| match v {
                        Value::Array(a) => Some(a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()),
                        // a comma-separated string (the metadata panel's single text field)
                        Value::String(t) => Some(t.split(',').map(|s| s.trim().to_string()).collect()),
                        _ => None,
                    })
                };
                let targets = s.targets(p)?;
                // GPS: "lat, lon" (decimal or 51°30'26"N 0°7'39"W), [lat, lon], or null / "" to clear
                let gps: Option<Option<(f64, f64)>> = match p.get("gps") {
                    None => None,
                    Some(Value::Null) => Some(None),
                    Some(Value::String(t)) if t.trim().is_empty() => Some(None),
                    Some(Value::String(t)) => Some(Some(
                        parse_gps(t).ok_or_else(|| bad("photo.setMeta", format!("can't read `{t}` as coordinates (e.g. 51.5072, -0.1276)")))?,
                    )),
                    Some(Value::Array(a)) if a.len() == 2 => match (a[0].as_f64(), a[1].as_f64()) {
                        (Some(la), Some(lo)) if la.abs() <= 90.0 && lo.abs() <= 180.0 => Some(Some((la, lo))),
                        _ => return Err(bad("photo.setMeta", "gps is [latitude, longitude] in degrees")),
                    },
                    Some(_) => return Err(bad("photo.setMeta", "gps is \"lat, lon\", [lat, lon] or null")),
                };
                let status = match str_param(p, "copyrightStatus") {
                    Some(t) => Some(
                        CopyrightStatus::parse(t)
                            .ok_or_else(|| bad("photo.setMeta", format!("copyrightStatus `{t}`: unknown, copyrighted or publicDomain")))?,
                    ),
                    None => None,
                };
                let mut ops = Vec::new();
                for id in targets {
                    let Some(ph) = s.catalog.photo(id) else { continue };
                    let mut m = ph.meta.clone();
                    if let Some(g) = gps {
                        m.gps = g;
                    }
                    if let Some(st) = status {
                        m.copyright_status = st;
                    }
                    for (k, field) in [
                        ("title", &mut m.title),
                        ("headline", &mut m.headline),
                        ("caption", &mut m.caption),
                        ("descriptionWriter", &mut m.description_writer),
                        ("category", &mut m.category),
                        ("copyright", &mut m.copyright),
                        ("usageTerms", &mut m.usage_terms),
                        ("copyrightUrl", &mut m.copyright_url),
                        ("creator", &mut m.creator),
                        ("location", &mut m.location),
                        ("city", &mut m.city),
                        ("state", &mut m.state),
                        ("country", &mut m.country),
                        ("countryCode", &mut m.country_code),
                        ("altText", &mut m.alt_text),
                        ("extendedDescription", &mut m.extended_description),
                    ] {
                        if let Some(v) = str_param(p, k) {
                            *field = v.to_string();
                        }
                    }
                    for k in lcc_meta::CreatorContact::KEYS {
                        if let (Some(v), Some(field)) = (str_param(p, k), m.contact.get_mut(k)) {
                            *field = v.to_string();
                        }
                    }
                    // keywords are stored cleaned (`a | b ` is `a|b`), each once whatever its case
                    use lcc_catalog::keywords::clean;
                    // (a set of the lower-cased keywords keeps this linear however many are supplied; upstream #3709)
                    let mut seen: std::collections::HashSet<String> = m.keywords.iter().map(|x| x.to_lowercase()).collect();
                    let add = |m: &mut lcc_catalog::Meta, seen: &mut std::collections::HashSet<String>, k: &str| {
                        let k = clean(k);
                        if !k.is_empty() && seen.insert(k.to_lowercase()) {
                            m.keywords.push(k);
                        }
                    };
                    if let Some(k) = strs("keywords") {
                        m.keywords.clear();
                        seen.clear();
                        for k in k {
                            add(&mut m, &mut seen, &k);
                        }
                    }
                    for k in strs("addKeywords").unwrap_or_default() {
                        add(&mut m, &mut seen, &k);
                    }
                    let removed: std::collections::HashSet<String> =
                        strs("removeKeywords").unwrap_or_default().iter().map(|k| clean(k).to_lowercase()).collect();
                    if !removed.is_empty() {
                        m.keywords.retain(|x| !removed.contains(&clean(x).to_lowercase()));
                    }
                    // supplemental categories and subject codes: a list, each once (the UI edits
                    // them as one comma-separated text field, so trimming empty entries matters)
                    let distinct = |v: Vec<String>| {
                        let mut out: Vec<String> = Vec::new();
                        for s in v {
                            if !s.is_empty() && !out.contains(&s) {
                                out.push(s);
                            }
                        }
                        out
                    };
                    if let Some(v) = strs("supplementalCategories") {
                        m.supplemental_categories = distinct(v);
                    }
                    if let Some(v) = strs("subjectCode") {
                        m.subject_code = distinct(v);
                    }
                    ops.push(Op::SetMeta { id, meta: Box::new(m) });
                }
                let n = ops.len();
                s.commit("Edit Info", Op::Batch { ops })?;
                // keywords just added become the Recent Keywords set
                let added: Vec<String> = strs("addKeywords").unwrap_or_default().into_iter().chain(strs("keywords").unwrap_or_default()).collect();
                if n > 0 && !added.is_empty() {
                    crate::cmd::keywords::note_recent(s, &added);
                }
                Ok(json!({"changed": n}))
            }
        ),
        // ---- albums
        cmd!(
            "album.create",
            "New Collection",
            ["File"],
            None,
            "{name, parent?: folderId, folder?: bool, addSelected?: bool} → {id, skipped} (skipped: Local photos of the selection, which albums can't hold)",
            always,
            |s, p| {
                let name = str_param(p, "name").unwrap_or("Untitled Collection").trim().to_string();
                if name.is_empty() {
                    return Err(bad("album.create", "empty name"));
                }
                let folder = bool_or(p, "folder", false);
                let parent = p.get("parent").and_then(Value::as_u64).map(AlbumId);
                let (mut photos, skipped) = if !folder && bool_or(p, "addSelected", false) { album_targets(s, s.selected()) } else { (vec![], 0) };
                // in the order they are shown (a Custom Order or sort carries over), not selection order
                if photos.len() > 1 {
                    let pos: std::collections::HashMap<PhotoId, usize> = s.visible().iter().enumerate().map(|(i, p)| (*p, i)).collect();
                    photos.sort_by_key(|p| pos.get(p).copied().unwrap_or(usize::MAX));
                }
                let id = s.catalog.alloc_album_id();
                let cover = photos.first().copied();
                s.commit(
                    if folder { "New Folder" } else { "New Collection" },
                    Op::AddAlbum {
                        album: Album {
                            id,
                            name,
                            parent,
                            folder,
                            photos,
                            cover,
                            smart: None,
                            quick: false,
                            order: None,
                            creation: None,
                            photo_order: Vec::new(),
                        },
                    },
                )?;
                Ok(json!({"id": id.0, "skipped": skipped}))
            }
        ),
        cmd!(
            "album.duplicate",
            "Duplicate Collection",
            [],
            None,
            "{id, name?}: a new regular album beside it with the same photos and Custom Order (a second order of the same photos) → {id}",
            always,
            |s, p| {
                const C: &str = "album.duplicate";
                let id = p.get("id").and_then(Value::as_u64).map(AlbumId).ok_or_else(|| bad(C, "missing `id`"))?;
                let a = s.catalog.album(id).cloned().ok_or_else(|| bad(C, format!("no collection {}", id.0)))?;
                if a.folder || a.smart.is_some() || a.quick {
                    return Err(bad(C, "only a regular collection can be duplicated"));
                }
                let name =
                    str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{} copy", a.name));
                let new = s.catalog.alloc_album_id();
                let photo_order = s.catalog.album_photo_order(id);
                let album = Album {
                    id: new,
                    name,
                    parent: a.parent,
                    folder: false,
                    photos: a.photos.clone(),
                    cover: a.cover,
                    smart: None,
                    quick: false,
                    order: None,
                    creation: None,
                    photo_order,
                };
                s.commit("Duplicate Collection", Op::AddAlbum { album })?;
                Ok(json!({"id": new.0}))
            }
        ),
        cmd!(
            "album.createSmart",
            "New Smart Collection…",
            [],
            None,
            "{name, rules?: partial Filter (rating, ratingOp, flag, label, kind, edited, keyword, camera, lens, dateFrom, dateTo, date, text, album, ruleSet: {match: all|any|none, rules: [{field, op, value} | {group: ruleSet}]}; see album.ruleFields), parent?: folderId}: without `rules`, saves the current view (source + filter)",
            always,
            |s, p| {
                let name = str_param(p, "name").unwrap_or("Smart Collection").trim().to_string();
                if name.is_empty() {
                    return Err(bad("album.createSmart", "empty name"));
                }
                let rules = match p.get("rules") {
                    Some(r) => merge_rules(&lcc_catalog::Filter::default(), r, "album.createSmart", &s.catalog, None)?,
                    None => view_rules(s),
                };
                let parent = p.get("parent").and_then(Value::as_u64).map(AlbumId);
                let id = s.catalog.alloc_album_id();
                let album = Album { parent, smart: Some(Box::new(rules)), ..Album::new(id, name) };
                s.commit("New Smart Collection", Op::AddAlbum { album })?;
                Ok(json!({"id": id.0, "count": s.catalog.album_count(id)}))
            }
        ),
        cmd!(query "album.ruleFields", "Smart Collection Rule Fields", [], None, "{} → [{field, label, kind (text|keywords|number|date|choice|bool|album: an album id), group (field-menu submenu, null at the top level), ops: [{op, label}], choices? (ids), choiceLabels? ({id: label})}] for ruleSet rules, in menu order", always, |_, _| {
            use lcc_catalog::rules::{FIELDS, Kind, field_group, ops_for};
            Ok(json!(FIELDS
                .iter()
                .map(|(id, label, kind)| {
                    let k = match kind {
                        Kind::Text => "text",
                        Kind::Keywords => "keywords",
                        Kind::Number => "number",
                        Kind::Album => "album",
                        Kind::Date => "date",
                        Kind::Choice(_) => "choice",
                        Kind::Bool => "bool",
                    };
                    let mut v = json!({"field": id, "label": label, "kind": k, "group": field_group(id), "ops": ops_for(*kind).iter().map(|(o, l)| json!({"op": o, "label": l})).collect::<Vec<_>>()});
                    if let Kind::Choice(c) = kind {
                        v["choices"] = json!(c.iter().map(|c| c.0).collect::<Vec<_>>());
                        v["choiceLabels"] = c.iter().map(|(id, label)| (id.to_string(), json!(label))).collect::<serde_json::Map<_, _>>().into();
                    }
                    v
                })
                .collect::<Vec<_>>()))
        }),
        cmd!(
            "album.setRules",
            "Edit Smart Collection",
            [],
            None,
            "{id, rules?: partial Filter merged onto the current rules (null clears a field), replace?: bool, fromView?: bool (use the current view), name?: also rename it (blank keeps the name); one undo step, both or neither}",
            always,
            |s, p| {
                let id = album_param(p, "id", "album.setRules")?;
                let cur =
                    s.catalog.album(id).and_then(|a| a.smart.as_deref().cloned()).ok_or_else(|| bad("album.setRules", "not a smart collection"))?;
                let rules = if bool_or(p, "fromView", false) {
                    // the view is saved as it is, but never as a loop (showing A, which tests this
                    // album, and saving that view here would make it test itself)
                    let view = view_rules(s);
                    let loops: Vec<String> = view
                        .rule_set
                        .as_ref()
                        .map(|r| r.check_for(&s.catalog, Some(id)))
                        .unwrap_or_default()
                        .into_iter()
                        .chain(s.catalog.album_filter_problem(&view, Some(id)))
                        .filter(|p| p.issue == lcc_catalog::rules::Issue::AlbumLoop)
                        .map(|p| p.to_string())
                        .collect();
                    if !loops.is_empty() {
                        return Err(bad("album.setRules", loops.join("; ")));
                    }
                    view
                } else {
                    let replace = bool_or(p, "replace", false);
                    let (folder, album) = (cur.library_folder.clone(), cur.album);
                    let base = if replace { Default::default() } else { cur };
                    let mut r = merge_rules(&base, p.get("rules").unwrap_or(&Value::Null), "album.setRules", &s.catalog, Some(id))?;
                    // the rules dialog has no folder field: replacing its rules keeps the folder
                    // unless the call says (`libraryFolder: null`) to drop it
                    if replace && p.get("rules").and_then(|r| r.get("libraryFolder")).is_none() {
                        r.library_folder = folder;
                    }
                    // nor one for the album a saved view is limited to
                    // (`album: null` drops it). One that is gone is let go of: this is where
                    // the mark on such an album sends the person
                    if replace && p.get("rules").and_then(|r| r.get("album")).is_none() {
                        r.album = album.filter(|a| s.catalog.album(*a).is_some());
                    }
                    r
                };
                let set = Op::SetAlbumRules { id, rules: Box::new(rules) };
                // a new name goes with the rules: one step, applied (and undone) together
                let rename = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty() && s.catalog.album(id).is_some_and(|a| a.name != *n));
                match rename {
                    Some(name) => s.commit("Edit Smart Collection", Op::Batch { ops: vec![Op::RenameAlbum { id, name: name.to_string() }, set] })?,
                    None => s.commit("Edit Smart Collection", set)?,
                };
                Ok(json!({"count": s.catalog.album_count(id)}))
            }
        ),
        cmd!("album.rename", "Rename Collection", [], None, "{id, name}", always, |s, p| {
            let id = album_param(p, "id", "album.rename")?;
            let name = str_param(p, "name").ok_or_else(|| bad("album.rename", "missing name"))?.to_string();
            s.commit("Rename Collection", Op::RenameAlbum { id, name })?;
            ok()
        }),
        cmd!(
            "album.delete",
            "Delete Collection",
            [],
            None,
            "{id}: deletes the collection or folder and its descendants, keeping photos in the library; one undo step",
            always,
            |s, p| {
                let id = album_param(p, "id", "album.delete")?;
                let folder = s.catalog.album(id).ok_or_else(|| bad("album.delete", "no such collection"))?.folder;
                let mut children = std::collections::BTreeMap::<AlbumId, Vec<AlbumId>>::new();
                for album in s.catalog.albums() {
                    if let Some(parent) = album.parent {
                        children.entry(parent).or_default().push(album.id);
                    }
                }
                // Iterative traversal: even a malformed cycle is rejected before any change.
                // Reverse parent-first order removes children first; the batch inverse restores
                // parents first, including memberships, smart rules, covers and manual ordering.
                let mut pending = vec![id];
                let mut order = Vec::new();
                let mut removed = std::collections::BTreeSet::new();
                while let Some(next) = pending.pop() {
                    if !removed.insert(next) {
                        return Err(bad("album.delete", "cycle in collection folders"));
                    }
                    order.push(next);
                    if let Some(nested) = children.remove(&next) {
                        pending.extend(nested);
                    }
                }
                let ops = order.into_iter().rev().map(|id| Op::RemoveAlbum { id }).collect();
                // a deleted album that was being shown gives way to All Photos, as after any
                // other way it goes (`Session::reconcile_view`)
                s.commit(if folder { "Delete Folder" } else { "Delete Collection" }, Op::Batch { ops })?;
                ok()
            }
        ),
        cmd!("album.move", "Move Collection", [], None, "{id, parent?: folderId|null}", always, |s, p| {
            let id = album_param(p, "id", "album.move")?;
            let parent = p.get("parent").and_then(Value::as_u64).map(AlbumId);
            s.commit("Move Collection", Op::MoveAlbum { id, parent })?;
            ok()
        }),
        cmd!(
            "album.reorder",
            "Reorder Collection",
            [],
            None,
            "{id, parent?: folderId|null, before?: albumId|null}: places the album (or folder) in `parent` (default: where it is; null: the top level) before the sibling `before` of the same kind, or last of its kind; the folder is then ordered by hand. One undo step. → {changed}: how many edits it took (0: already there)",
            always,
            |s, p| {
                let id = album_param(p, "id", "album.reorder")?;
                let me = s.catalog.album(id).ok_or_else(|| bad("album.reorder", "no such collection"))?;
                let parent = if p.get("parent").is_some() { opt_album_param(p, "parent", "album.reorder")? } else { me.parent };
                let before = opt_album_param(p, "before", "album.reorder")?;
                if before == Some(id) {
                    // before itself: where it is
                    return Ok(json!({"changed": 0}));
                }
                let mut order: Vec<AlbumId> = s.catalog.album_children(parent).into_iter().filter(|a| a.id != id).map(|a| a.id).collect();
                let at = match before {
                    Some(b) => {
                        let at = order.iter().position(|x| *x == b).ok_or_else(|| bad("album.reorder", "`before` is not inside that folder"))?;
                        if s.catalog.album(b).is_none_or(|x| x.folder != me.folder) {
                            return Err(bad("album.reorder", "folders are placed among folders and collections among collections"));
                        }
                        at
                    }
                    None if me.folder => order.iter().take_while(|x| s.catalog.album(**x).is_some_and(|a| a.folder)).count(),
                    None => order.len(),
                };
                order.insert(at.min(order.len()), id);
                let moved = me.parent != parent;
                let mut ops = Vec::new();
                if moved {
                    ops.push(Op::MoveAlbum { id, parent });
                }
                for (i, aid) in order.iter().enumerate() {
                    let want = u32::try_from(i).ok();
                    // a moved album starts without a place in its new folder
                    let have = if moved && *aid == id { None } else { s.catalog.album(*aid).and_then(|a| a.order) };
                    if have != want {
                        ops.push(Op::SetAlbumOrder { id: *aid, order: want });
                    }
                }
                if ops.is_empty() {
                    return Ok(json!({"changed": 0}));
                }
                let changed = ops.len();
                s.commit("Reorder Collection", Op::Batch { ops })?;
                Ok(json!({"changed": changed}))
            }
        ),
        cmd!(
            "album.sort",
            "Sort Collections by Name",
            [],
            None,
            "{parent?: folderId|null}: drops the hand order of the albums inside the folder (default: the top level), so they are listed by name → {changed}: how many albums lost their place",
            always,
            |s, p| {
                let parent = opt_album_param(p, "parent", "album.sort")?;
                let ops: Vec<Op> = s
                    .catalog
                    .album_children(parent)
                    .into_iter()
                    .filter(|a| a.order.is_some())
                    .map(|a| Op::SetAlbumOrder { id: a.id, order: None })
                    .collect();
                if ops.is_empty() {
                    return Ok(json!({"changed": 0}));
                }
                let changed = ops.len();
                s.commit("Sort Collections by Name", Op::Batch { ops })?;
                Ok(json!({"changed": changed}))
            }
        ),
        cmd!(
            "album.addPhotos",
            "Add to Collection",
            ["Photo"],
            None,
            "{id: albumId, ids?: [photoIds]} (default: selection) → {added, skipped}; Local photos are skipped (albums hold library photos), only Local ones is an error; ids the library doesn't have are dropped; nothing new to add changes nothing (no undo step)",
            has_library_selection,
            |s, p| {
                let id = album_param(p, "id", "album.addPhotos")?;
                let targets = super::photo_ids(p, "album.addPhotos")?.unwrap_or_else(|| s.selected());
                let al = s.catalog.album(id).ok_or_else(|| bad("album.addPhotos", "no such collection"))?;
                if al.is_smart() || al.folder {
                    return Err(bad("album.addPhotos", "smart collections and folders can't hold photos"));
                }
                let (targets, skipped) = album_targets_or_err(s, "album.addPhotos", targets)?;
                let mut photos = al.photos.clone();
                let before = photos.len();
                // looked up, not searched for: adding thousands to a big album stays linear
                let mut held: std::collections::HashSet<PhotoId> = photos.iter().copied().collect();
                for t in targets {
                    if held.insert(t) {
                        photos.push(t);
                    }
                }
                let added = photos.len() - before;
                // all of them were there already (or none was left to add): the album is unchanged,
                // so no undo step, and redo stays
                if added == 0 {
                    return Ok(json!({"added": 0, "skipped": skipped}));
                }
                let cover = al.cover.or(photos.first().copied());
                s.commit("Add to Collection", Op::Batch { ops: vec![Op::SetAlbumPhotos { id, photos }, Op::SetAlbumCover { id, cover }] })?;
                Ok(json!({"added": added, "skipped": skipped}))
            }
        ),
        cmd!(
            "album.orderPhotos",
            "Move in Custom Order",
            [],
            None,
            "{album?: albumId (default: the album shown), ids: [photoIds], before?: photoId (default: to the end)}: moves the photos, in their current order, before `before`; sorts the view by Custom Order → {order}",
            always,
            |s, p| {
                const C: &str = "album.orderPhotos";
                let a = match opt_album_param(p, "album", C)? {
                    Some(a) => a,
                    None => custom_album(s).ok_or_else(|| bad(C, "missing `album` (no album is shown)"))?,
                };
                if !s.catalog.has_custom_order(a) {
                    return Err(bad(C, "only regular collections have a custom order"));
                }
                // `before` means "left of it in the grid": a descending Custom Order shows the saved order
                // reversed, so the drop is worked out on the shown list and saved back in forward order
                let reversed = custom_album(s) == Some(a) && s.sort.key == SortKey::Custom && !s.sort.ascending;
                let mut base = shown_order(s, a);
                if reversed {
                    base.reverse();
                }
                let moved = order_ids(s, a, p, C)?;
                if moved.is_empty() {
                    return Err(bad(C, "missing `ids`"));
                }
                let before = match p.get("before") {
                    None | Some(Value::Null) => None,
                    Some(v) => {
                        let b = v.as_u64().map(PhotoId).ok_or_else(|| bad(C, "`before` must be a photo id or null"))?;
                        if !base.contains(&b) {
                            return Err(bad(C, format!("photo {} is not in the collection", b.0)));
                        }
                        Some(b)
                    }
                };
                // the moved photos keep their order among themselves
                let moving: std::collections::HashSet<PhotoId> = moved.iter().copied().collect();
                let in_order: Vec<PhotoId> = base.iter().copied().filter(|x| moving.contains(x)).collect();
                // dropping before one of the moved photos: before the first photo after it that stays
                let anchor = before.and_then(|b| base.iter().skip_while(|x| **x != b).find(|x| !moving.contains(x)).copied());
                let mut order: Vec<PhotoId> = base.iter().copied().filter(|x| !moving.contains(x)).collect();
                let at = anchor.and_then(|b| order.iter().position(|x| *x == b)).unwrap_or(order.len());
                order.splice(at..at, in_order);
                if reversed {
                    order.reverse();
                }
                set_custom_order(s, a, order, "Move in Custom Order")
            }
        ),
        cmd!(
            "album.setPhotoOrder",
            "Set Custom Order",
            [],
            None,
            "{album: albumId, ids: [photoIds]}: the album's Custom Order (repeats ignored; members not listed follow in their current order) → {order}",
            always,
            |s, p| {
                const C: &str = "album.setPhotoOrder";
                let a = album_param(p, "album", C)?;
                if !s.catalog.has_custom_order(a) {
                    return Err(bad(C, "only regular collections have a custom order"));
                }
                let mut order = order_ids(s, a, p, C)?;
                let listed: std::collections::HashSet<PhotoId> = order.iter().copied().collect();
                order.extend(s.catalog.album_photo_order(a).into_iter().filter(|x| !listed.contains(x)));
                set_custom_order(s, a, order, "Set Custom Order")
            }
        ),
        cmd!(
            "album.toggleTarget",
            "Add to Target Collection",
            [],
            None,
            "{ids?}: B: adds the photos to the target album (the Quick Collection unless one is set), or removes them when they're all in it → {album, added, toggled, skipped}; Local photos are skipped, ids the library doesn't have are dropped (none left: nothing changes and `added` is null)",
            has_library_selection,
            |s, p| {
                let targets =
                    album_targets_or_err(s, "album.toggleTarget", super::photo_ids(p, "album.toggleTarget")?.unwrap_or_else(|| s.selected()))?;
                let (targets, skipped) = targets;
                // nothing left to toggle (every id named is gone): no album is made or changed
                if targets.is_empty() {
                    let existing = s
                        .target_album
                        .filter(|a| s.catalog.album(*a).is_some_and(|al| !al.is_smart() && !al.folder))
                        .or_else(|| s.catalog.quick_collection());
                    let al = existing.and_then(|a| s.catalog.album(a));
                    return Ok(
                        json!({"album": al.map(|a| a.id.0), "name": al.map(|a| a.name.clone()), "added": null, "toggled": 0, "count": al.map_or(0, |a| s.catalog.album_count(a.id)), "skipped": skipped}),
                    );
                }
                let target = s.target_album.filter(|a| s.catalog.album(*a).is_some_and(|al| !al.is_smart() && !al.folder));
                let id = match target.or_else(|| s.catalog.quick_collection()) {
                    Some(id) => id,
                    None => {
                        let id = s.catalog.alloc_album_id();
                        let album = Album { quick: true, ..Album::new(id, "Quick Collection") };
                        s.commit("New Quick Collection", Op::AddAlbum { album })?;
                        id
                    }
                };
                let al = s.catalog.album(id).ok_or_else(|| bad("album.toggleTarget", "no target collection"))?;
                // looked up, not searched for: thousands toggled in a big album stay linear
                let mut held: std::collections::HashSet<PhotoId> = al.photos.iter().copied().collect();
                let all_in = targets.iter().all(|t| held.contains(t));
                let photos: Vec<PhotoId> = if all_in {
                    let targets: std::collections::HashSet<PhotoId> = targets.iter().copied().collect();
                    al.photos.iter().copied().filter(|x| !targets.contains(x)).collect()
                } else {
                    al.photos.iter().copied().chain(targets.iter().copied().filter(|t| held.insert(*t))).collect()
                };
                let name = al.name.clone();
                // photos that went in or came out: what a toast can say (not the ones that were in already)
                let toggled = al.photos.len().abs_diff(photos.len());
                let cover = al.cover.filter(|c| photos.contains(c)).or(photos.first().copied());
                s.commit(
                    if all_in { "Remove from Target Collection" } else { "Add to Target Collection" },
                    Op::Batch { ops: vec![Op::SetAlbumPhotos { id, photos }, Op::SetAlbumCover { id, cover }] },
                )?;
                Ok(json!({"album": id.0, "name": name, "added": !all_in, "toggled": toggled, "count": s.catalog.album_count(id), "skipped": skipped}))
            }
        ),
        cmd!("album.setTarget", "Set as Target Collection", [], None, "{id: albumId | null} (null = the Quick Collection)", always, |s, p| {
            s.target_album = match p.get("id").and_then(Value::as_u64) {
                Some(a) => {
                    let id = AlbumId(a);
                    let al = s.catalog.album(id).ok_or_else(|| bad("album.setTarget", "no such collection"))?;
                    if al.is_smart() || al.folder {
                        return Err(bad("album.setTarget", "smart collections and folders can't hold photos"));
                    }
                    Some(id)
                }
                None => None,
            };
            Ok(json!({"target": s.target_album.map(|a| a.0)}))
        }),
        cmd!("album.clearQuick", "Clear Quick Collection", [], None, "{}", always, |s, _| {
            let Some(id) = s.catalog.quick_collection() else { return ok() };
            s.commit(
                "Clear Quick Collection",
                Op::Batch { ops: vec![Op::SetAlbumPhotos { id, photos: vec![] }, Op::SetAlbumCover { id, cover: None }] },
            )?;
            ok()
        }),
        cmd!(
            "library.showQuickCollection",
            "Show Quick Collection",
            [],
            None,
            "{}: the grid shows the Quick Collection → {count}",
            always,
            |s, _| { s.execute("library.source", &json!({"kind": "quickCollection"})) }
        ),
        cmd!(
            "album.saveQuick",
            "Save Quick Collection…",
            [],
            None,
            "{name?: album name (default \"Quick Collection\"), clear?: bool (default true)}: a new album holding the Quick Collection's photos, which is then cleared; one undo step → {id, count}",
            always,
            |s, p| {
                let quick = s.catalog.quick_collection();
                let photos: Vec<PhotoId> = quick.and_then(|q| s.catalog.album(q)).map(|a| a.photos.clone()).unwrap_or_default();
                if photos.is_empty() {
                    return Err(bad("album.saveQuick", "the Quick Collection is empty"));
                }
                let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Quick Collection");
                let id = s.catalog.alloc_album_id();
                let album = Album::new(id, name);
                let cover = photos.first().copied();
                let mut ops = vec![Op::AddAlbum { album }, Op::SetAlbumPhotos { id, photos: photos.clone() }, Op::SetAlbumCover { id, cover }];
                if let Some(q) = quick.filter(|_| p.get("clear").and_then(Value::as_bool).unwrap_or(true)) {
                    ops.push(Op::SetAlbumPhotos { id: q, photos: vec![] });
                    ops.push(Op::SetAlbumCover { id: q, cover: None });
                }
                s.commit("Save Quick Collection", Op::Batch { ops })?;
                Ok(json!({"id": id.0, "count": photos.len()}))
            }
        ),
        cmd!(
            "library.autoImport",
            "Auto Import Settings",
            [],
            None,
            "{folder?: path | null (clear), enabled?: bool (switch watching on or off, keeping the folder; choosing a folder switches it on), copy?: bool (copy into the library's Originals, else add in place), album?: name | null, subfolders?: bool (also watch the folder's subfolders), rename?: file-name template for copies (empty keeps names), preset?: develop preset id | null, metadataPreset?: name | null, keywords?: [name] or comma-separated text}: a watched folder whose new photos are added as they arrive (library.autoImportScan; the app scans every few seconds) → the settings",
            always,
            |s, p| {
                if let Some(f) = p.get("folder") {
                    s.import_defaults.auto_folder = match f.as_str().map(str::trim).filter(|f| !f.is_empty()) {
                        Some(f) if std::path::Path::new(f).is_dir() || cfg!(target_arch = "wasm32") => Some(f.to_string()),
                        Some(f) => return Err(bad("library.autoImport", format!("`{f}` is not a folder"))),
                        None => None,
                    };
                    if s.import_defaults.auto_folder.is_some() {
                        s.import_defaults.auto_paused = false;
                    }
                }
                if let Some(on) = p.get("enabled").and_then(Value::as_bool) {
                    s.import_defaults.auto_paused = !on;
                }
                if let Some(c) = p.get("copy").and_then(Value::as_bool) {
                    s.import_defaults.auto_copy = c;
                }
                if let Some(a) = p.get("album") {
                    s.import_defaults.auto_album = a.as_str().map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
                }
                if let Some(b) = p.get("subfolders").and_then(Value::as_bool) {
                    s.import_defaults.auto_options.subfolders = b;
                }
                if let Some(r) = p.get("rename").and_then(Value::as_str) {
                    s.import_defaults.auto_options.rename = r.trim().to_string();
                }
                if let Some(v) = p.get("preset") {
                    let id = v.as_str().map(str::trim).filter(|x| !x.is_empty());
                    if let Some(id) = id
                        && !s.presets.iter().any(|x| x.id == id)
                    {
                        return Err(bad("library.autoImport", format!("unknown preset `{id}`")));
                    }
                    s.import_defaults.auto_options.preset = id.map(str::to_string);
                }
                if let Some(v) = p.get("metadataPreset") {
                    let n = v.as_str().map(str::trim).filter(|x| !x.is_empty());
                    if let Some(n) = n
                        && !s.metadata_presets.iter().any(|m| m.name.eq_ignore_ascii_case(n))
                    {
                        return Err(bad("library.autoImport", format!("unknown metadata preset `{n}`")));
                    }
                    s.import_defaults.auto_options.metadata_preset = n.map(str::to_string);
                }
                if let Some(k) = p.get("keywords") {
                    let list: Vec<String> = match k {
                        Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                        Value::String(t) => t.split(',').map(str::to_string).collect(),
                        _ => Vec::new(),
                    };
                    let mut kept: Vec<String> = Vec::new();
                    for k in list.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
                        if !kept.iter().any(|x| x.eq_ignore_ascii_case(k)) {
                            kept.push(k.to_string());
                        }
                    }
                    s.import_defaults.auto_options.keywords = kept;
                }
                s.save_prefs()?;
                let d = &s.import_defaults;
                Ok(json!({"folder": d.auto_folder, "enabled": d.auto_folder.is_some() && !d.auto_paused, "copy": d.auto_copy, "album": d.auto_album,
                    "subfolders": d.auto_options.subfolders, "rename": d.auto_options.rename, "preset": d.auto_options.preset,
                    "metadataPreset": d.auto_options.metadata_preset, "keywords": d.auto_options.keywords}))
            }
        ),
        cmd!(
            "library.autoImportScan",
            "Auto Import Now",
            [],
            None,
            "{listing?: [[path, size]] (the folder as listed by the caller, e.g. on a worker thread), start?: bool (false: return the `library.import` params as `import` instead of importing)}: add the watched folder's new photos (files the library doesn't have yet; partial / still-copying files wait for the next scan) → {imported, folder, warnings?: [{path, code, profile?, mode?, fields?}], import?}",
            always,
            |s, p| {
                let Some(folder) = s.import_defaults.auto_active_folder().cloned() else { return Ok(json!({"imported": [], "folder": null})) };
                let scope = s.auto_import_scope();
                // only files that stopped growing: a file still being written is left for later
                let known: std::collections::HashSet<String> = s
                    .catalog
                    .photos()
                    .filter_map(|p| match &p.source {
                        lcc_catalog::Source::File { path } => Some(path.clone()),
                        _ => None,
                    })
                    .collect();
                // the folder's files and sizes: listed here, or already listed on a worker thread (the app)
                let listing: Vec<(String, u64)> = match p.get("listing").and_then(Value::as_array) {
                    Some(l) => l.iter().filter_map(|e| Some((e.get(0)?.as_str()?.to_string(), e.get(1)?.as_u64()?))).collect(),
                    None => match &scope {
                        Some(sc) => sc.list(),
                        None => list_auto_import_folder(&folder),
                    }
                    .map_err(|e| bad("library.autoImportScan", e))?,
                };
                let mut fresh = Vec::new();
                for (ps, size) in listing {
                    if known.contains(&ps) {
                        continue;
                    }
                    // each file is tried once (a non-photo isn't retried every scan)
                    if s.auto_import_seen.get(&ps) == Some(&u64::MAX) {
                        continue;
                    }
                    let seen = s.auto_import_seen.insert(ps.clone(), size);
                    if size > 0 && seen == Some(size) {
                        fresh.push(ps);
                    }
                }
                if fresh.is_empty() {
                    return Ok(json!({"imported": [], "folder": folder}));
                }
                let mode = if s.import_defaults.auto_copy { "copy" } else { "add" };
                // a photo deleted from the library doesn't come back on its own because its file
                // is still in the watched folder (a copy's source is not a known path)
                let mut params = json!({"paths": fresh, "mode": mode, "onDeleted": "skip"});
                let o = s.import_defaults.auto_options.clone();
                // (a copy's names; an add keeps the files where and as they are)
                if !o.rename.trim().is_empty() && s.import_defaults.auto_copy {
                    params["rename"] = json!(o.rename.trim());
                }
                // a preset that was deleted meanwhile is left out, not a failed import every scan
                if let Some(id) = o.preset.as_ref().filter(|id| s.presets.iter().any(|p| p.id == **id)) {
                    params["preset"] = json!(id);
                }
                if let Some(n) = o.metadata_preset.as_ref().filter(|n| s.metadata_presets.iter().any(|m| m.name.eq_ignore_ascii_case(n))) {
                    params["metadataPreset"] = json!(n);
                }
                if !o.keywords.is_empty() {
                    params["keywords"] = json!(o.keywords);
                }
                if let Some(a) = s.import_defaults.auto_album.clone() {
                    match s.catalog.albums().find(|al| al.name.eq_ignore_ascii_case(&a) && !al.folder && !al.is_smart()) {
                        Some(al) => params["album"] = json!(al.id.0),
                        None => params["albumName"] = json!(a),
                    }
                }
                for f in &fresh {
                    s.auto_import_seen.insert(f.clone(), u64::MAX);
                }
                if p.get("start").and_then(Value::as_bool) == Some(false) {
                    // the caller imports them (the app: on a worker thread)
                    return Ok(json!({"imported": [], "folder": folder, "import": params}));
                }
                let sel = s.selection.clone();
                let r = s.execute("library.import", &params)?;
                // arriving photos don't take over the selection
                s.selection = sel;
                let mut result = json!({"imported": r["imported"], "folder": folder});
                if let Some(warnings) = r.get("warnings") {
                    result["warnings"] = warnings.clone();
                }
                Ok(result)
            }
        ),
        cmd!(
            "album.removePhotos",
            "Remove from Collection",
            ["Photo"],
            None,
            "{id?: albumId (default: the album on screen), ids?}: takes the photos out of the album only: they stay in the library and in their folders (issue #1805; photo.delete removes them from the library) → {removed, album: name}",
            has_selection,
            |s, p| {
                let id = match p.get("id") {
                    Some(_) => album_param(p, "id", "album.removePhotos")?,
                    None => match s.source {
                        LibrarySource::Album(a) => a,
                        _ => return Err(bad("album.removePhotos", "no album: pass `id` or show an album")),
                    },
                };
                let targets = super::photo_ids(p, "album.removePhotos")?.unwrap_or_else(|| s.selected());
                let al = s.catalog.album(id).ok_or_else(|| bad("album.removePhotos", "no such collection"))?;
                if al.is_smart() {
                    return Err(bad("album.removePhotos", "smart collections update automatically: change their rules"));
                }
                if al.folder {
                    return Err(bad("album.removePhotos", "collection folders don't hold photos"));
                }
                let name = al.name.clone();
                let targets: std::collections::HashSet<PhotoId> = targets.into_iter().collect();
                let photos: Vec<PhotoId> = al.photos.iter().copied().filter(|x| !targets.contains(x)).collect();
                let removed = al.photos.len().saturating_sub(photos.len());
                if removed > 0 {
                    s.commit("Remove from Collection", Op::SetAlbumPhotos { id, photos })?;
                }
                Ok(json!({"removed": removed, "album": name}))
            }
        ),
        cmd!("album.setCover", "Set as Collection Cover", [], None, "{id: albumId, photo?: photoId}", has_active, |s, p| {
            let id = album_param(p, "id", "album.setCover")?;
            let photo = crate::Session::photo_param(p, "photo", "album.setCover")?.or(s.active());
            s.commit("Set Collection Cover", Op::SetAlbumCover { id, cover: photo })?;
            ok()
        }),
        // ---- import
        cmd!(query "library.inspectLightroom", "Inspect Lightroom Catalog", [], None,
        "{path: .lrcat} → {photos, collections, missing, warnings}; read-only, no Lightroom required",
        always, |_, p| {
            let path = str_param(p,"path").ok_or_else(|| bad("library.inspectLightroom","missing path"))?;
            let data = crate::lightroom_catalog::read(std::path::Path::new(path)).map_err(|e| bad("library.inspectLightroom",e))?;
            Ok(json!({"photos":data.photos.len(),"collections":data.collections.iter().filter(|r|r.get("systemOnly").and_then(Value::as_f64).unwrap_or(0.0)==0.0).count(),"missing":data.photos.iter().filter(|p| !p.original_is_file()).map(|p|&p.path).collect::<Vec<_>>(),"warnings":data.warnings,"skipped":data.skipped}))
        }),
        cmd!(
            "library.importLightroom",
            "Import Lightroom Catalog",
            [],
            None,
            "{path: .lrcat, updateExisting?: false}; direct import of paths, ratings, flags, metadata, keywords, collections, virtual copies & mapped edits. Archives source settings/history first. Existing edited photos are preserved unless updateExisting=true; source catalog is read-only.",
            always,
            |s, p| {
                let path = str_param(p, "path").ok_or_else(|| bad("library.importLightroom", "missing path"))?;
                crate::lightroom_catalog::import(s, std::path::Path::new(path), bool_or(p, "updateExisting", false))
            }
        ),
        cmd!(
            query "library.importPreview",
            "Review Import",
            [],
            None,
            "{paths: [file or folder (recursive)]} → {candidates: [{path, name, format, kind, width, height, fileSize, captured, duplicate?: path|content, existing?, error?, previewOnly?: why a raw can only be shown from its embedded preview}], duplicates, scanned, scanLimited: bool (the folder walk stopped at its scan limit: there may be more photos), skipped: {unreadable (folders that could not be listed), unsupported (files in the folders that are not a supported photo type, XMP sidecars not counted; a file named in paths is always tried)}}: nothing is added",
            always,
            |s, p| {
                let paths = strs(p, "paths");
                if paths.is_empty() {
                    return Err(bad("library.importPreview", "no paths"));
                }
                let out = crate::import::scan_report(s, &paths);
                let dups = out.candidates.iter().filter(|c| c.duplicate.is_some()).count();
                Ok(json!({
                    "scanned": out.candidates.len(),
                    "duplicates": dups,
                    "candidates": out.candidates,
                    "scanLimited": out.truncated,
                    "skipped": {"unreadable": out.unreadable, "unsupported": out.unsupported},
                }))
            }
        ),
        cmd!(
            "library.import",
            "Import Photos",
            ["File"],
            Some("Cmd+Shift+I"),
            "{paths: [file or folder (recursive)], mode?: add|copy|move (add = reference the files in place; copy = into the library's Originals/YYYY/YYYY-MM-DD/; move = as copy, then each original and its XMP sidecars are removed from the source, only after the copy is verified (a hard link on the same volume, else copied, synced and compared byte for byte) and its catalog record is saved; failed, duplicate and unchecked files keep their sources; a taken name gets -1, -2…; undo removes the photos from the library but leaves the files at the destination), destination?: folder for copies / moves, organize?: date (YYYY/YYYY-MM-DD) | month (YYYY/YYYY-MM) | flat | a folder template, e.g. `{date:%Y}/{date:%Y%m%d}` → 2026/20260114 (the template's `/` make the folders, each level expanded with the rename tokens and made a safe folder name: never outside the destination; must be relative, no `..`; a level with missing metadata is `unknown`); dated by capture time, else the import time, rename?: file-name template for copies, original extension added (tokens: {name} {num} {seq} {seq:N} {date} {date:%Y%m%d} {folder} {camera} {lens} {iso} {rating} {title} {creator} {ext}; photo.renameTokens explains each; blank = keep names), renameStart?: 1 (must fit this platform; later files are reported failed if the sequence runs out), metadataPreset?: name, dng?: bool (copy raws as DNG; copy only), local?: bool (browsing: the photos stay out of the library, like library.browse; not with move), album?: albumId, albumName?: new album, preset?: presetId, presetProfile?: bool (a preset's camera profile replaces the raw's; default false: each raw keeps its body's camera profile through the presets), presets?: [presetId] (a stack applied in order after `preset`, one History step each; a browsed photo joining the library gets the presets, keywords and metadata preset too, on top of its own culling and edits, and copied or moved its record follows its file), recipe?: [{id: a plain develop control (develop.controls; no white balance, geometry or crop), value}] (a batch-edit recipe: each control is set to its value, clamped to the control's range, on every photo this import adds, after its sidecar, camera default and preset, as one extra History step named Import adjustments; a photo the library already has (a duplicate, one restored from Recently Deleted, one joined from a browsed folder) is not changed; a control a recipe may not name is refused before anything is imported; preset.saveRecipe keeps a recipe for reuse), keywords?: [..], cameraProfile?: auto (default: each body's own camera profile, else the camera's default) | {all: camera profile id} (every raw of that profile's camera) | {perBody: {body (as profiles.bodies lists it): camera profile id}}, for this import only; a raw whose XMP sidecar sets develop settings keeps its own, onDeleted?: restore|skip|fresh (a file that is in Recently Deleted: restore (default) = bring the photo back with its edits, relinked to the file when its own is gone; skip = leave it there and say so, fresh = delete the trashed record and import the file as new), importDuplicates?: bool (false by default = Lightroom's Don't Import Suspected Duplicates: a file whose content the library already has is skipped; true imports it as a new photo; a path already in the library, or a photo in Recently Deleted, is still handled as above)} → {imported, duplicates: [{path, existing, reason, existingDeleted?}], restored?: [photoId], failed, warnings?: [{path, code: cameraProfile|uprightGeometry|unmappedXmp, profile?, mode?: off|auto|level|vertical|full|guided|unknown, fields?}], moved?: [{from, to, sidecars?}], kept?: [{path, reason}] (move: sources left in place and why), album?}",
            always,
            |s, p| {
                let req = import_params(s, p)?;
                if req.paths.is_empty() {
                    return Err(bad("library.import", "no paths"));
                }
                let undo0 = s.undo.len();
                let report = crate::import::import_with(s, &req.paths, &req.opts)?;
                let report = import_batch_done(s, report, req.album, req.album_name.as_deref())?;
                // one undo step for the whole import
                let ids_of = |k: &str| -> Vec<u64> { report[k].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default() };
                let (imported, restored) = (ids_of("imported"), ids_of("restored"));
                let n = s.undo.len().saturating_sub(undo0);
                let (verb, count) = if imported.is_empty() && !restored.is_empty() { ("Restore", restored.len()) } else { ("Add", imported.len()) };
                s.merge_undo(n, &format!("{verb} {count} Photo{}", if count == 1 { "" } else { "s" }));
                let imported = if imported.is_empty() { restored } else { imported };
                if let Some(f) = imported.first() {
                    s.selection = Selection::single(PhotoId(*f));
                }
                Ok(report)
            }
        ),
        // ---- persistence
        cmd!(query "library.info", "Catalog Info", [], None, "{}", always, |s, _| {
            let photos = s.catalog.len();
            let albums = s.catalog.albums().count();
            let (rendered_n, rendered_bytes) = s.media.rendered.mem_usage();
            let (sources_n, sources_bytes) = s.media.source_usage();
            let disk = s.media.rendered.disk().map(|d| {
                use std::sync::atomic::Ordering::Relaxed;
                json!({
                    "path": d.dir().display().to_string(),
                    "bytes": d.size(),
                    "hits": d.hits.load(Relaxed),
                    "misses": d.misses.load(Relaxed),
                    "writes": d.writes.load(Relaxed),
                })
            });
            let cache = json!({
                "renderedInMemory": rendered_n, "renderedBytes": rendered_bytes,
                "sourcesInMemory": sources_n, "sourceBytes": sources_bytes,
                "disk": disk,
            });
            let Some(lib) = &s.library else {
                return Ok(json!({"persistent": false, "photos": photos, "albums": albums, "cache": cache}));
            };
            let j = lib.journal();
            Ok(json!({
                "persistent": true,
                "path": lib.dir.display().to_string(),
                "photos": photos,
                "albums": albums,
                "seq": j.seq(),
                "snapshotSeq": j.snapshot_seq(),
                "logRecords": j.log_records(),
                "logBytes": j.log_bytes(),
                "lastError": lib.last_error,
                // settings files that were unreadable or damaged at open (kept, defaults used)
                "settingsWarnings": lib.settings_warnings,
                // changes applied in memory whose write failed (retried by every save)
                "unsavedOps": s.unsaved().map_or(0, |u| u.0),
                "unsavedError": s.unsaved().map(|u| u.1),
                // untouched Local records forgotten when the library opened
                "forgotLocal": lib.forgot_local.as_ref().map(|p| json!({"forgotten": p.evict.len(), "local": p.local, "keptRecent": p.kept_recent, "keptTouched": p.kept_touched, "keptInUse": p.kept_in_use})),
                "persistence": j.stats(),
                "cache": cache,
                "load": {
                    "created": lib.report.created,
                    "replayed": lib.report.replayed,
                    "tornBytes": lib.report.torn_bytes,
                    "damaged": lib.report.damaged,
                    // the catalog was in an older format: upgraded, a verified copy kept as it was
                    "upgradedFrom": lib.report.upgraded_from,
                    "upgradeBackup": lib.report.upgrade_backup.as_ref().map(|p| p.display().to_string()),
                },
            }))
        }),
        cmd!("library.compact", "Optimize Catalog", ["File", "Libraries"], None, "{}", has_library, |s, _| {
            s.compact_library()?;
            ok()
        }),
        cmd!("library.clearPreviews", "Discard Preview Cache", ["Library", "Previews"], None, "{}", always, |s, _| {
            s.media.rendered.clear();
            s.media.clear_sources();
            ok()
        }),
    ]
}

/// `base` with a partial Filter (JSON) merged on top.
/// `patch` merged onto `base`, its rules brought up to date (`RuleSet::upgrade`), unchecked (see
/// [`merge_rules`]).
fn merge_filter(base: &lcc_catalog::Filter, patch: &Value, c: &str) -> Result<lcc_catalog::Filter> {
    let mut v = serde_json::to_value(base).unwrap_or_default();
    lcc_develop::presets::deep_merge(&mut v, patch);
    let mut f: lcc_catalog::Filter = serde_json::from_value(v).map_err(|e| bad(c, e.to_string()))?;
    if let Some(rules) = f.rule_set.as_mut() {
        rules.upgrade();
    }
    Ok(f)
}

/// `patch` merged onto `base`, refused when the rule set it sets has problems (`RuleSet::check_for`;
/// `owner` is the smart album the rules are for, so testing an album that leads back to it is a
/// problem too).
fn merge_rules(
    base: &lcc_catalog::Filter,
    patch: &Value,
    c: &str,
    cat: &lcc_catalog::Catalog,
    owner: Option<AlbumId>,
) -> Result<lcc_catalog::Filter> {
    let f = merge_filter(base, patch, c)?;
    // only rules this change sets are checked: one that stopped checking since (its album was
    // deleted) doesn't block editing the album's other settings
    let sets_rules = patch.get("ruleSet").is_some_and(|r| !r.is_null());
    let mut problems = f.rule_set.as_ref().filter(|_| sets_rules).map(|r| r.check_for(cat, owner)).unwrap_or_default();
    // and a loop through the album field the change sets
    if patch.get("album").is_some_and(|a| !a.is_null()) {
        problems.extend(cat.album_filter_problem(&f, owner));
    }
    if !problems.is_empty() {
        return Err(bad(c, problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")));
    }
    Ok(f)
}

/// Whether `path` lies above a disk: some of its `ids` photos are on a disk (not the startup
/// disk) that the folder does not lie in. Such a folder (`/Volumes`, `/mnt`…) is not a folder of
/// one disk but a way to reach several.
pub(crate) fn covers_other_disks(s: &Session, path: &str, ids: &[PhotoId]) -> bool {
    ids.iter()
        .filter_map(|id| s.catalog.photo(*id))
        .filter_map(|p| lcc_catalog::folders::volume_of(p))
        .any(|v| v != "/" && !lcc_catalog::query::folder_within(path, &v))
}

/// The current view (source + filter) as smart-album rules. Viewing a smart album starts from
/// its rules with the filter bar's settings on top.
fn view_rules(s: &Session) -> lcc_catalog::Filter {
    use lcc_catalog::Filter;
    let smart = match s.source {
        LibrarySource::Album(a) => s.catalog.album(a).and_then(|a| a.smart.as_deref().cloned()),
        _ => None,
    };
    let mut rules = match smart {
        Some(base) => {
            // overlay the fields the filter bar changed
            let cur = serde_json::to_value(&s.filter).unwrap_or_default();
            let def = serde_json::to_value(Filter::default()).unwrap_or_default();
            let mut patch = serde_json::Map::new();
            if let (Some(c), Some(d)) = (cur.as_object(), def.as_object()) {
                for (k, v) in c {
                    if d.get(k) != Some(v) {
                        patch.insert(k.clone(), v.clone());
                    }
                }
            }
            // the rating rule is its stars and operator together: "= no stars" (the unrated photos) has the default stars,
            // which alone would leave the album's own with "=" (upstream #3433)
            if s.filter.rating_filters() {
                patch.insert("rating".into(), cur["rating"].clone());
                patch.insert("ratingOp".into(), cur["ratingOp"].clone());
            }
            // the view as it is: a rule that no longer checks (its album was deleted) just matches
            // nothing there, and mustn't cost the filter bar's settings
            merge_filter(&base, &Value::Object(patch), "").unwrap_or(base)
        }
        None => {
            let mut f = s.source.to_filter(&s.filter, &s.catalog);
            f.deleted = false;
            if s.source == LibrarySource::LibraryFolder {
                f.library_folder = s.library_folder.clone();
            }
            f
        }
    };
    // a view of a smart album limited to an album that is gone is saved without that limit
    if rules.album.is_some_and(|a| s.catalog.album(a).is_none()) {
        rules.album = None;
    }
    rules
}

impl Session {
    /// The current view (source + filter) as smart-album rules (see `album.createSmart`).
    pub fn view_rules(&self) -> lcc_catalog::Filter {
        view_rules(self)
    }
}

fn has_library(s: &Session) -> std::result::Result<(), String> {
    if s.library.is_some() { Ok(()) } else { Err("no catalog is open (in-memory session)".into()) }
}

/// Coordinates typed by a person: "51.5072, -0.1276", "51.5072 N 0.1276 W",
/// "51°30'26\"N 0°7'39\"W" (degrees, minutes, seconds; N/S/E/W or signs).
pub(crate) fn parse_gps(t: &str) -> Option<(f64, f64)> {
    // typographic minus signs and dashes mean a negative value, like ASCII '-'
    let t = t.trim();
    if t.is_empty() || t.chars().take(129).count() > 128 {
        return None;
    }
    let normalized = t.replace(['\u{2212}', '\u{2012}', '\u{2013}', '\u{FE63}', '\u{FF0D}'], "-");
    let t = normalized.as_str();

    if let Some((a, b)) = t.split_once(',') {
        if b.contains(',') {
            return None;
        }
        return parse_gps_pair(a, b);
    }

    // A suffix N/S marks the end of latitude; a prefix N/S uses the next hemisphere prefix as
    // the boundary. If only longitude has a hemisphere, the whitespace pass below handles it.
    let mut parsed: Option<(f64, f64)> = None;
    let mut latitude_markers = t.char_indices().filter(|(_, c)| matches!(c, 'N' | 'S' | 'n' | 's'));
    if let Some((first, marker)) = latitude_markers.next() {
        if first == 0 {
            if let Some((second, _)) = t.char_indices().skip(1).find(|(_, c)| is_gps_hemisphere(*c))
                && let Some(pair) = parse_gps_pair(t.get(..second)?, t.get(second..)?)
            {
                parsed = Some(pair);
            }
        } else {
            let end = first.checked_add(marker.len_utf8())?;
            return parse_gps_pair(t.get(..end)?, t.get(end..)?);
        }
    }

    // A comma-free pair can be separated by whitespace. Trying each boundary also permits spaces
    // between DMS components without letting unconsumed text vanish. If multiple boundaries parse,
    // they must agree so ambiguous text cannot be reinterpreted.
    for (at, separator) in t.char_indices().filter(|(_, c)| c.is_whitespace()) {
        let after = at.checked_add(separator.len_utf8())?;
        if let Some(pair) = parse_gps_pair(t.get(..at)?, t.get(after..)?) {
            match parsed {
                Some(previous) if previous != pair => return None,
                None => parsed = Some(pair),
                Some(_) => {}
            }
        }
    }
    parsed
}

#[derive(Clone, Copy)]
enum GpsAxis {
    Latitude,
    Longitude,
}

fn is_gps_hemisphere(c: char) -> bool {
    matches!(c, 'N' | 'S' | 'E' | 'W' | 'n' | 's' | 'e' | 'w')
}

fn parse_gps_pair(latitude: &str, longitude: &str) -> Option<(f64, f64)> {
    Some((parse_gps_coordinate(latitude, GpsAxis::Latitude)?, parse_gps_coordinate(longitude, GpsAxis::Longitude)?))
}

fn parse_gps_coordinate(text: &str, axis: GpsAxis) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let prefix = ['N', 'S', 'E', 'W']
        .into_iter()
        .find_map(|h| text.strip_prefix(h).or_else(|| text.strip_prefix(h.to_ascii_lowercase())).map(|rest| (h, rest)));
    let suffix = ['N', 'S', 'E', 'W']
        .into_iter()
        .find_map(|h| text.strip_suffix(h).or_else(|| text.strip_suffix(h.to_ascii_lowercase())).map(|rest| (rest, h)));
    let (body, hemisphere) = if let Some((h, body)) = prefix {
        (body, Some(h))
    } else if let Some((body, h)) = suffix {
        (body, Some(h))
    } else {
        (text, None)
    };
    if body.chars().any(is_gps_hemisphere) {
        return None;
    }
    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let hemisphere_negative = match (axis, hemisphere) {
        (_, None) => None,
        (GpsAxis::Latitude, Some('N')) | (GpsAxis::Longitude, Some('E')) => Some(false),
        (GpsAxis::Latitude, Some('S')) | (GpsAxis::Longitude, Some('W')) => Some(true),
        _ => return None,
    };

    let mut components = Vec::with_capacity(3);
    let mut token = String::new();
    let mut explicit_sign = None;
    let mut degree_mark = false;
    let mut minute_mark = false;
    let mut second_mark = false;

    for c in body.chars() {
        match c {
            '0'..='9' | '.' => token.push(c),
            '+' | '-' if components.is_empty() && token.is_empty() => token.push(c),
            c if c.is_whitespace() => flush_gps_number(&mut token, &mut components, &mut explicit_sign)?,
            '°' => {
                flush_gps_number(&mut token, &mut components, &mut explicit_sign)?;
                if components.len() != 1 || degree_mark || minute_mark || second_mark {
                    return None;
                }
                degree_mark = true;
            }
            '\'' | '′' => {
                flush_gps_number(&mut token, &mut components, &mut explicit_sign)?;
                if components.len() != 2 || minute_mark || second_mark {
                    return None;
                }
                minute_mark = true;
            }
            '"' | '″' => {
                flush_gps_number(&mut token, &mut components, &mut explicit_sign)?;
                if components.len() != 3 || !minute_mark || second_mark {
                    return None;
                }
                second_mark = true;
            }
            _ => return None,
        }
    }
    flush_gps_number(&mut token, &mut components, &mut explicit_sign)?;

    let [degrees, rest @ ..] = components.as_slice() else { return None };
    if rest.iter().any(|v| *v < 0.0 || *v >= 60.0) {
        return None;
    }
    let magnitude = degrees.abs() + rest.first().copied().unwrap_or(0.0) / 60.0 + rest.get(1).copied().unwrap_or(0.0) / 3600.0;
    if !magnitude.is_finite() {
        return None;
    }

    let negative = match (hemisphere_negative, explicit_sign) {
        (Some(hemisphere_negative), Some(explicit_negative)) if hemisphere_negative != explicit_negative => return None,
        (Some(hemisphere_negative), _) => hemisphere_negative,
        (None, Some(explicit_negative)) => explicit_negative,
        (None, None) => false,
    };
    let value = if negative { -magnitude } else { magnitude };
    let limit = match axis {
        GpsAxis::Latitude => 90.0,
        GpsAxis::Longitude => 180.0,
    };
    (value.is_finite() && value.abs() <= limit).then_some(value)
}

fn flush_gps_number(token: &mut String, components: &mut Vec<f64>, explicit_sign: &mut Option<bool>) -> Option<()> {
    if token.is_empty() {
        return Some(());
    }
    if components.len() == 3 {
        return None;
    }
    let value = token.parse::<f64>().ok()?;
    if !value.is_finite() {
        return None;
    }
    if components.is_empty() && (token.starts_with('-') || token.starts_with('+')) {
        *explicit_sign = Some(token.starts_with('-'));
    }
    components.push(value);
    token.clear();
    Some(())
}

#[cfg(test)]
mod gps_tests {
    use serde_json::json;

    #[test]
    fn coordinates_people_type() {
        // coordinates people type
        {
            let p = super::parse_gps;
            assert_eq!(p("51.5072, -0.1276"), Some((51.5072, -0.1276)));
            assert_eq!(p("10 20 30"), None);
            assert_eq!(p("10  20"), Some((10.0, 20.0)));
            let (la, lo) = p("51°30'26\"N 0°7'39\"W").unwrap();
            assert!((la - 51.50722).abs() < 1e-4 && (lo + 0.1275).abs() < 1e-4, "{la} {lo}");
            let (la, lo) = p("33.8688 S, 151.2093 E").unwrap();
            assert!((la + 33.8688).abs() < 1e-9 && (lo - 151.2093).abs() < 1e-9);
            assert_eq!(p("51.5 N 0.1 W"), Some((51.5, -0.1)));
            assert_eq!(p("N 51.5072 W 0.1276"), Some((51.5072, -0.1276)));
            assert_eq!(p("S 33.8688 E 151.2093"), Some((-33.8688, 151.2093)));
            assert_eq!(p("\u{2212}33.8688, 151.2093"), Some((-33.8688, 151.2093)));
            assert_eq!(p("51.5072 -0.1276"), Some((51.5072, -0.1276)));
            assert_eq!(p("51.5072 0.1276 W"), Some((51.5072, -0.1276)));
            let (la, lo) = p("51° 30' 26\" 0° 7' 39\"").unwrap();
            assert!((la - 51.50722).abs() < 1e-4 && (lo - 0.1275).abs() < 1e-4, "{la} {lo}");
            assert_eq!(p("95, 10"), None);
            assert_eq!(p("hello"), None);
            assert_eq!(p("s 33.8688 e 151.2093"), Some((-33.8688, 151.2093)));
            assert_eq!(p("0N, 0E"), Some((0.0, 0.0)));
            assert_eq!(p("90N, 180E"), Some((90.0, 180.0)));
            assert_eq!(p("90S, 180W"), Some((-90.0, -180.0)));
        }
        // gps does not reinterpret exponents
        {
            assert_eq!(super::parse_gps("1e-3, 2e-3"), None);
        }
        // gps rejects wrong axis or conflicting hemispheres
        {
            let p = super::parse_gps;
            assert_eq!(p("45E, 7N"), None);
            assert_eq!(p("45E 7N"), None);
            assert_eq!(p("45NS, 7E"), None);
            assert_eq!(p("-45N, 7E"), None);
            assert_eq!(p("45N 7NE"), None);
        }
        // gps tries whitespace split when prefix marker split fails
        {
            assert_eq!(super::parse_gps("N51 0E"), Some((51.0, 0.0)));
        }
        // gps rejects unconsumed coordinate text
        {
            assert_eq!(super::parse_gps("garbage45, 7junk"), None);
        }
        // gps rejects oversized coordinate text
        {
            let input = format!("{}, 0", "0".repeat(129));
            assert_eq!(super::parse_gps(&input), None);
        }
        // gps rejects invalid dms components
        {
            let p = super::parse_gps;
            assert_eq!(p("45°60'0\"N, 7E"), None);
            assert_eq!(p("45°0'60\"N, 7E"), None);
            assert_eq!(p("45°-1'0\"N, 7E"), None);
            assert_eq!(p("45°0'-1\"N, 7E"), None);
        }
    }

    #[test]
    fn gps_rejected_text_changes_nothing_and_a_valid_edit_is_one_undo_step() {
        // gps rejected text does not change metadata
        {
            let mut s = crate::Session::with_demo();
            let ids: Vec<_> = s.catalog.photos().take(2).map(|photo| photo.id).collect();
            assert_eq!(ids.len(), 2, "demo has two records for the batch check");
            let id_values: Vec<_> = ids.iter().map(|id| id.0).collect();
            s.execute("library.select", &json!({"ids": id_values})).unwrap();
            s.execute("photo.setMeta", &json!({"title": "Before"})).unwrap();
            s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.25})).unwrap();
            s.execute("edit.undo", &json!({})).unwrap();
            assert!(!s.redo.is_empty(), "rejection must preserve a populated redo history");

            let snapshot = s.catalog.to_snapshot();
            let revision = s.catalog.revision;
            let selection = s.selection.clone();
            let commits = s.commits();
            let undo = format!("{:?}", s.undo);
            let redo = format!("{:?}", s.redo);
            let error = s.execute("photo.setMeta", &json!({"gps": "garbage45, 7junk", "title": "After"})).unwrap_err();

            assert!(matches!(error, crate::EngineError::BadParams { ref cmd, .. } if cmd == "photo.setMeta"), "{error}");
            assert_eq!(s.catalog.to_snapshot(), snapshot, "catalog data is unchanged");
            assert_eq!(s.catalog.revision, revision, "catalog revision is unchanged");
            assert_eq!(s.selection, selection, "selection is unchanged");
            assert_eq!(s.commits(), commits, "commit count is unchanged");
            assert_eq!(format!("{:?}", s.undo), undo, "undo history is unchanged");
            assert_eq!(format!("{:?}", s.redo), redo, "redo history is unchanged");
            for id in ids {
                assert_eq!(s.catalog.photo(id).map(|photo| photo.meta.title.as_str()), Some("Before"));
            }
        }
        // gps valid edit is one undo step
        {
            let mut s = crate::Session::with_demo();
            let id = s.active().or_else(|| s.catalog.photos().next().map(|photo| photo.id)).unwrap();
            let previous = s.catalog.photo(id).unwrap().meta.clone();
            let commits = s.commits();
            s.execute("photo.setMeta", &json!({"gps": "48°51'30\"N 2°17'40\"E", "title": "GPS edited"})).unwrap();
            let photo = s.catalog.photo(id).unwrap();
            assert_eq!(photo.meta.title, "GPS edited");
            assert!(photo.meta.gps.is_some());
            assert_eq!(s.commits(), commits + 1);

            s.execute("edit.undo", &json!({})).unwrap();
            let photo = s.catalog.photo(id).unwrap();
            assert_eq!(photo.meta.title, previous.title);
            assert_eq!(photo.meta.gps, previous.gps);
        }
    }
}

#[cfg(test)]
#[path = "library/tests_relative_ratings.rs"]
mod relative_rating_tests;

#[cfg(test)]
#[path = "library/tests_flag_cycle.rs"]
mod flag_cycle_tests;
#[cfg(test)]
#[path = "../tests_custom_order.rs"]
mod tests_custom_order;

#[cfg(test)]
#[path = "library/tests_rated_out.rs"]
mod rated_out_tests;

#[cfg(test)]
#[path = "library/tests_step_selection.rs"]
mod step_selection_tests;

#[cfg(test)]
#[path = "library/tests_next_unrated.rs"]
mod next_unrated_tests;

#[cfg(test)]
#[path = "library/tests_clear_label.rs"]
mod clear_label_tests;

#[cfg(test)]
#[path = "library/tests_enable_filters.rs"]
mod enable_filters_tests;

#[cfg(test)]
#[path = "library/tests_source_album_filter.rs"]
mod source_album_filter_tests;

#[cfg(test)]
#[path = "library/tests_select.rs"]
mod tests_select;

#[cfg(test)]
#[path = "library/tests_hide_rejected.rs"]
mod hide_rejected_tests;

#[cfg(test)]
#[path = "library/tests_source_history.rs"]
mod source_history_tests;

#[cfg(test)]
#[path = "library/tests_advanced_search.rs"]
mod advanced_search_tests;
