//! Library commands: view source, filter/sort, selection, ratings/flags/labels, rotate, delete,
//! metadata, albums, import.

use lightcraft_catalog::{Album, AlbumId, ColorLabel, CopyrightStatus, Filter, Flag, GroupBy, Op, PhotoId, Sort, SortKey};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, has_active, has_selection, ok, str_param};
use crate::{LibrarySource, Result, Selection, Session};

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
    let rd = std::fs::read_dir(folder).map_err(|e| format!("{folder}: {e}"))?;
    Ok(rd
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let m = std::fs::metadata(e.path()).ok()?;
            m.is_file().then(|| (e.path().to_string_lossy().to_string(), m.len()))
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
    let album = p.get("album").and_then(Value::as_u64);
    if let Some(a) = album
        && s.catalog.album(AlbumId(a)).is_none_or(|al| al.folder || al.is_smart())
    {
        return Err(bad("library.import", "album must be a regular album"));
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
            crate::import::OnDeleted::parse(v).ok_or_else(|| bad("library.import", format!("unknown onDeleted `{v}` (skip|restore|fresh)")))?
        }
        None => Default::default(),
    };
    let opts = crate::import::ImportOptions {
        on_deleted,
        mode,
        preset,
        keywords: strs(p, "keywords"),
        destination: str_param(p, "destination").map(str::to_string),
        organize,
        rename: str_param(p, "rename").map(str::to_string),
        rename_start: p.get("renameStart").and_then(Value::as_u64).unwrap_or(1) as usize,
        metadata_preset,
        convert_dng: bool_or(p, "dng", false),
        local: bool_or(p, "local", false),
    };
    let album_name = str_param(p, "albumName").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    Ok(ImportRequest { paths, opts, album, album_name })
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

fn ids_param(p: &Value) -> Option<Vec<PhotoId>> {
    p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(PhotoId).collect())
}

/// Apply one op per target as a single undo step.
fn for_targets(s: &mut Session, p: &Value, label: &str, f: impl Fn(PhotoId) -> Option<Op>) -> Result<Value> {
    let targets = s.targets(p);
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
    for id in s.targets(p) {
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
    advance_if(s, p);
    Ok(json!({"changed": n}))
}

fn advance_if(s: &mut Session, p: &Value) {
    if bool_or(p, "advance", false) {
        let _ = step(s, 1);
    }
}

fn step(s: &mut Session, d: isize) -> Result<Value> {
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

fn rotate(s: &mut Session, p: &Value, cw: bool) -> Result<Value> {
    let targets = s.targets(p);
    let mut ops = Vec::new();
    for id in targets {
        if let Some(d) = s.develop_of(id) {
            let mut d = (*d).clone();
            d.orientation = d.orientation.rotated(cw);
            ops.extend(s.develop_op(id, d, "Rotate"));
        }
    }
    let n = ops.len();
    s.commit(if cw { "Rotate Right" } else { "Rotate Left" }, Op::Batch { ops })?;
    Ok(json!({"changed": n}))
}

fn flip(s: &mut Session, p: &Value, horizontal: bool) -> Result<Value> {
    let targets = s.targets(p);
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
fn select_by(s: &mut Session, p: &Value) -> Result<Value> {
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
    let t = now.bytes().fold(0u64, |h, b| lightcraft_catalog::mix64(h ^ u64::from(b)));
    (lightcraft_catalog::mix64(prev.wrapping_add(1)) ^ t) & (MAX_SEED - 1)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        // ---- view source / filter / sort
        cmd!(
            "library.source",
            "Show Source",
            [],
            None,
            "{kind: all|recentlyAdded|album|recentlyDeleted|picks|missing|libraryFolder, id?: albumId (an album, a smart album, or a folder of albums: the photos of every album inside it, each once), path?: a path from library.folders (for libraryFolder)}",
            always,
            |s, p| {
                let kind = str_param(p, "kind").unwrap_or("all");
                s.source = match kind {
                    "all" => LibrarySource::All,
                    "recentlyAdded" => LibrarySource::RecentlyAdded,
                    "recentlyDeleted" => LibrarySource::RecentlyDeleted,
                    "picks" => LibrarySource::Picks,
                    "missing" => LibrarySource::Missing,
                    "album" => {
                        let a = album_param(p, "id", "library.source")?;
                        // a folder of albums is a source too: what the albums inside it hold
                        if s.catalog.album(a).is_none() {
                            return Err(bad("library.source", "no such album"));
                        }
                        LibrarySource::Album(a)
                    }
                    "libraryFolder" => {
                        let path = str_param(p, "path").filter(|d| !d.trim().is_empty() && !lightcraft_catalog::query::folder_key(d).is_empty());
                        let path = path.ok_or_else(|| bad("library.source", "libraryFolder needs the `path` of a folder from library.folders"))?;
                        if lightcraft_catalog::folders::is_startup_disk(path) {
                            return Err(bad("library.source", "the startup disk's path covers every disk: choose a folder in it"));
                        }
                        let f = Filter { library_folder: Some(path.to_string()), ..Default::default() };
                        let ids = s.catalog.query(&f, &Sort::default());
                        if ids.is_empty() {
                            return Err(bad("library.source", format!("{path}: no photo in the library was imported from it")));
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
                let vis = s.visible_cloned();
                if s.selection.active.is_none_or(|a| !vis.contains(&a)) {
                    s.selection = vis.first().map(|f| Selection::single(*f)).unwrap_or_default();
                }
                Ok(json!({"count": vis.len()}))
            }
        ),
        cmd!(
            "library.filter",
            "Filter",
            [],
            None,
            "partial Filter: {text?, rating?, ratingOp?: atLeast|exactly|atMost, flag?: pick|reject|none|null, label?, kind?, merged?: hdr|panorama|hdrPanorama|any, edited?, date?, libraryFolder?: a path from library.folders, keyword?, person?, camera?}",
            always,
            |s, p| {
                let mut v = serde_json::to_value(&s.filter).unwrap_or_default();
                lightcraft_develop::presets::deep_merge(&mut v, p);
                let f: Filter = serde_json::from_value(v).map_err(|e| bad("library.filter", e.to_string()))?;
                let mut f = f;
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
                    return Err(bad("library.filter", "no such album"));
                }
                s.filter = f;
                Ok(json!({"count": s.visible().len()}))
            }
        ),
        cmd!(
            query "library.folders",
            "Library Folders",
            [],
            None,
            "{} → [{name, path, count, own, volume, selectable, label?, children}] the disks and the folders the library's photos were imported from, with photo counts (subfolders included in `count`); show one with library.source {kind: libraryFolder, path} (rows with selectable false only open)",
            always,
            |s, _| Ok(serde_json::to_value(s.catalog.folder_tree()).unwrap_or_default())
        ),
        cmd!(
            "library.removeFolder",
            "Remove Folder from Library",
            [],
            None,
            "{path, disk?: bool} — move the library photos imported from this folder (and the folders inside it) to Recently Deleted; one undo step, no file is touched. A whole disk or share (`/Volumes/nas`, `C:\\`, `\\\\srv\\share`) goes only with `disk: true`, the startup disk never → {removed}",
            always,
            |s, p| {
                const C: &str = "library.removeFolder";
                let path = str_param(p, "path").filter(|d| !d.trim().is_empty()).ok_or_else(|| bad(C, "missing `path`"))?;
                if lightcraft_catalog::query::folder_key(path).is_empty() {
                    return Err(bad(C, format!("{path}: not a folder")));
                }
                if lightcraft_catalog::folders::is_startup_disk(path) {
                    return Err(bad(C, "choose a folder, not the whole startup disk"));
                }
                if lightcraft_catalog::folders::is_disk_root(path) && !bool_or(p, "disk", false) {
                    return Err(bad(C, format!("{path} is a whole disk or share: pass `disk: true` to remove everything on it")));
                }
                let f = Filter { library_folder: Some(path.to_string()), ..Default::default() };
                let ids = s.catalog.query(&f, &Sort::default());
                if ids.is_empty() {
                    return Err(bad(C, format!("{path}: no photo in the library was imported from it")));
                }
                if covers_other_disks(s, path, &ids) && !bool_or(p, "disk", false) {
                    return Err(bad(C, format!("{path} holds whole disks: pass `disk: true` to remove everything on them")));
                }
                let ops = ids.iter().map(|id| Op::SetDeleted { id: *id, deleted: true }).collect();
                s.commit("Remove Folder from Library", Op::Batch { ops })?;
                if s.filter.library_folder.as_deref().is_some_and(|c| lightcraft_catalog::query::folder_within(c, path)) {
                    s.filter.library_folder = None;
                }
                // the folder being shown is gone: back to everything, as for a deleted album
                if s.library_folder.as_deref().is_some_and(|c| lightcraft_catalog::query::folder_within(c, path)) {
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
            "{path, label: red|yellow|green|blue|purple|none} — give a folder of the library (one its photos were imported from, see library.folders) a colour label, or take it off (also once its photos are gone); one undo step. The label follows the folder when it is renamed or moved with folder.rename / folder.move → {path, label}",
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
                    return Err(bad(C, format!("{path}: no photo in the library was imported from it")));
                }
                if label.is_none() && s.catalog.folder_record(path).is_none() {
                    return Ok(json!({"path": path, "label": label}));
                }
                let op = s.catalog.folder_label_op(path, label);
                s.commit(if label.is_some() { "Set Folder Color Label" } else { "Remove Folder Color Label" }, op)?;
                Ok(json!({"path": path, "label": label}))
            }
        ),
        cmd!("library.clearFilter", "Clear Filters", ["View"], None, "{}", always, |s, _| {
            s.filter = Default::default();
            Ok(json!({"count": s.visible().len()}))
        }),
        cmd!(
            "library.sort",
            "Sort",
            ["View", "Sort"],
            None,
            "{key?: captureDate|importDate|editDate|fileName|rating|fileSize|random, ascending?: bool, group?: auto|none|day|month|year, seed?: u64 (the shuffle `random` gives)}",
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
                if key == SortKey::Random && s.sort.key != SortKey::Random && !explicit {
                    // switching to Random is a fresh shuffle, not whatever seed was left behind
                    seed = next_seed(seed, &(s.clock)());
                }
                s.sort = Sort { key, ascending: bool_or(p, "ascending", s.sort.ascending), group, seed };
                ok()
            }
        ),
        cmd!(
            "library.shuffle",
            "Reshuffle",
            ["View", "Sort"],
            None,
            "{seed?: 0..2^53-1} — sort at random; without `seed` a new shuffle each time",
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
            "{ids: [id], active?: id, mode?: replace|add|toggle|range} — every id must be a photo of the library",
            always,
            |s, p| {
                let ids = ids_param(p).unwrap_or_default();
                let active = p.get("active").and_then(Value::as_u64).map(PhotoId);
                let mode = str_param(p, "mode").unwrap_or("replace");
                // An id that is not in the library is an error, and the selection stays as it was:
                // selecting it would otherwise report success and leave the session with no usable
                // active photo (#182).
                if let Some(id) = ids.iter().chain(active.as_ref()).find(|id| s.catalog.photo(**id).is_none()) {
                    return Err(bad("library.select", format!("no such photo {}", id.0)));
                }
                s.end_interaction()?;
                match mode {
                    "replace" => {
                        s.selection = Selection { ids: ids.clone(), active: active.or(ids.first().copied()) };
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
                    "range" => {
                        let vis = s.visible_cloned();
                        if let Some(id) = ids.first() {
                            s.selection.extend_to(*id, &vis);
                        }
                    }
                    other => return Err(bad("library.select", format!("unknown mode `{other}`"))),
                }
                s.active_mask = None;
                s.active_spot = None;
                Ok(json!({"selected": s.selection.ids.len()}))
            }
        ),
        cmd!("library.selectAll", "Select All", ["Edit"], Some("Cmd+A"), "{}", always, |s, _| {
            let vis = s.visible_cloned();
            s.selection = Selection { active: s.selection.active.filter(|a| vis.contains(a)).or(vis.first().copied()), ids: vis };
            Ok(json!({"selected": s.selection.ids.len()}))
        }),
        cmd!(query "library.devices", "Cameras and Cards", [], None, "{} → [{name, path (its DCIM folder), root}] — mounted volumes with a DCIM folder", always, |_, _| {
            Ok(serde_json::to_value(crate::devices::devices_now()).unwrap_or_default())
        }),
        cmd!(query "photo.copyMetadata", "Copy Metadata", ["Photo"], None, "{} — title, caption, copyright (notice, status, usage terms, info URL), creator, location and keywords of the active photo", has_active, |s, _| {
            let id = s.active().ok_or_else(|| bad("photo.copyMetadata", "no active photo"))?;
            let m = &s.catalog.photo(id).ok_or_else(|| bad("photo.copyMetadata", "no photo"))?.meta;
            let v = json!({"title": m.title, "caption": m.caption, "altText": m.alt_text, "extendedDescription": m.extended_description,
                "copyright": m.copyright, "copyrightStatus": m.copyright_status.id(), "usageTerms": m.usage_terms, "copyrightUrl": m.copyright_url,
                "creator": m.creator, "location": m.location, "city": m.city, "state": m.state, "country": m.country,
                "keywords": m.keywords});
            s.meta_clipboard = Some(v.clone());
            Ok(v)
        }),
        cmd!(
            "photo.pasteMetadata",
            "Paste Metadata",
            ["Photo"],
            None,
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
            "{flag?: pick|reject|none, rating?: 0..5, ratingOp?: gte|eq|lte (default gte), label?: red|…|none, add?: bool (extend the selection)} — among the photos in view",
            always,
            select_by
        ),
        cmd!("library.selectNone", "Deselect All", ["Edit"], Some("Cmd+Shift+A"), "{}", always, |s, _| {
            let a = s.selection.active;
            s.selection = Selection { ids: a.into_iter().collect(), active: a };
            ok()
        }),
        cmd!("library.next", "Next Photo", [], Some("Right"), "{}", always, |s, _| step(s, 1)),
        cmd!("library.previous", "Previous Photo", [], Some("Left"), "{}", always, |s, _| step(s, -1)),
        // ---- rating / flags / labels
        cmd!("photo.rate", "Set Rating", ["Photo", "Set Rating"], None, "{rating: 0..5, ids?, advance?: bool}", has_selection, |s, p| {
            let r = super::f64_req(p, "rating", "photo.rate")? as i64;
            if !(0..=5).contains(&r) {
                return Err(bad("photo.rate", "rating must be 0..5"));
            }
            let v = for_targets(s, p, "Set Rating", |id| Some(Op::SetRating { id, rating: r as u8 }))?;
            advance_if(s, p);
            Ok(v)
        }),
        cmd!("photo.decreaseRating", "Decrease Rating", [], None, "{ids?, advance?: bool}", has_selection, |s, p| {
            change_rating(s, p, -1, "Decrease Rating")
        }),
        cmd!("photo.increaseRating", "Increase Rating", [], None, "{ids?, advance?: bool}", has_selection, |s, p| {
            change_rating(s, p, 1, "Increase Rating")
        }),
        cmd!("photo.flag", "Set Flag", ["Photo", "Set Flag"], None, "{flag: pick|reject|none, ids?, advance?: bool}", has_selection, |s, p| {
            let f = str_param(p, "flag").and_then(Flag::parse).ok_or_else(|| bad("photo.flag", "flag must be pick|reject|none"))?;
            let v = for_targets(s, p, "Set Flag", |id| Some(Op::SetFlag { id, flag: f }))?;
            advance_if(s, p);
            Ok(v)
        }),
        cmd!("photo.pick", "Flag as Pick", ["Photo", "Set Flag"], Some("P"), "{ids?}", has_selection, |s, p| {
            for_targets(s, p, "Flag as Pick", |id| Some(Op::SetFlag { id, flag: Flag::Pick }))
        }),
        cmd!("photo.reject", "Flag as Reject", ["Photo", "Set Flag"], Some("X"), "{ids?}", has_selection, |s, p| {
            for_targets(s, p, "Flag as Reject", |id| Some(Op::SetFlag { id, flag: Flag::Reject }))
        }),
        cmd!("photo.unflag", "Unflag", ["Photo", "Set Flag"], Some("U"), "{ids?}", has_selection, |s, p| {
            for_targets(s, p, "Unflag", |id| Some(Op::SetFlag { id, flag: Flag::None }))
        }),
        cmd!(
            "photo.label",
            "Set Color Label",
            ["Photo", "Set Color Label"],
            None,
            "{label: red|yellow|green|blue|purple|none, ids?}",
            has_selection,
            |s, p| {
                let l = match str_param(p, "label") {
                    None | Some("none") => None,
                    Some(x) => Some(ColorLabel::parse(x).ok_or_else(|| bad("photo.label", "unknown label"))?),
                };
                for_targets(s, p, "Set Color Label", |id| Some(Op::SetLabel { id, label: l }))
            }
        ),
        // ---- orientation
        cmd!("photo.rotateLeft", "Rotate Left", ["Photo"], Some("Cmd+["), "{ids?}", has_selection, |s, p| rotate(s, p, false)),
        cmd!("photo.rotateRight", "Rotate Right", ["Photo"], Some("Cmd+]"), "{ids?}", has_selection, |s, p| rotate(s, p, true)),
        cmd!("photo.flipHorizontal", "Flip Horizontal", ["Photo"], None, "{ids?}", has_selection, |s, p| flip(s, p, true)),
        cmd!("photo.flipVertical", "Flip Vertical", ["Photo"], None, "{ids?}", has_selection, |s, p| flip(s, p, false)),
        // ---- face / pet regions
        cmd!(
            "photo.removeRegion",
            "Remove Face Box",
            [],
            None,
            "{id?, index} — remove one face / pet region (by its position in the photo's regions) from the photo in the catalog; undoable. The XMP sidecar is never rewritten for this, even with auto-write on, so reading the metadata from the file brings the region back",
            always,
            |s, p| {
                let id =
                    p.get("id").and_then(Value::as_u64).map(PhotoId).or_else(|| s.active()).ok_or_else(|| bad("photo.removeRegion", "no photo"))?;
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
            "{id?, index, rect: {x0, y0, x1, y1}} — set one face / pet region's box (normalized, in the photo's upright frame; clamped to the photo, at least 0.5 % each way) in the catalog; undoable. Like photo.removeRegion it never rewrites the XMP sidecar",
            always,
            |s, p| {
                const C: &str = "photo.setRegion";
                // a drag is one undo step however many frames it took: the caller commits once, on release
                let id = p.get("id").and_then(Value::as_u64).map(PhotoId).or_else(|| s.active()).ok_or_else(|| bad(C, "no photo"))?;
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
                region.rect = lightcraft_geom::Rect { x0, y0, x1, y1 };
                s.commit("Resize Face Box", Op::SetMeta { id, meta: Box::new(meta) })?;
                s.skip_auto_write = true;
                Ok(json!({"rect": {"x0": x0, "y0": y0, "x1": x1, "y1": y1}}))
            }
        ),
        // ---- delete / restore
        cmd!("photo.delete", "Delete Photo", ["Photo"], Some("Delete"), "{ids?} — moves to Recently Deleted", has_selection, |s, p| {
            let before = s.visible_cloned();
            let v = for_targets(s, p, "Delete", |id| Some(Op::SetDeleted { id, deleted: true }))?;
            select_after_deletion(s, &before);
            Ok(v)
        }),
        cmd!("photo.restore", "Restore", ["Photo"], None, "{ids?}", has_selection, |s, p| for_targets(s, p, "Restore", |id| Some(Op::SetDeleted {
            id,
            deleted: false
        }))),
        cmd!(
            "library.emptyRecentlyDeleted",
            "Empty Recently Deleted",
            ["Photo"],
            None,
            "{} → {deleted} — removes every photo in Recently Deleted from the library (files on disk stay)",
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
        cmd!("photo.deletePermanently", "Delete Permanently", ["Photo"], None, "{ids?}", has_selection, |s, p| {
            let before = s.visible_cloned();
            let t = s.targets(p);
            let op = s.catalog.delete_photos_permanently_ops(&t);
            s.commit("Delete Permanently", op)?;
            select_after_deletion(s, &before);
            Ok(json!({"deleted": t.len()}))
        }),
        // ---- metadata
        cmd!(
            "photo.setMeta",
            "Edit Info",
            [],
            None,
            "{ids?, title?, caption?, altText?, extendedDescription?, copyright?, copyrightStatus?: unknown|copyrighted|publicDomain, usageTerms?, copyrightUrl?, creator?, location?, city?, state?, country?, gps?: \"lat, lon\" | [lat, lon] | null, keywords?: [..], addKeywords?: [..], removeKeywords?: [..]}",
            has_selection,
            |s, p| {
                let strs =
                    |k: &str| p.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>());
                let targets = s.targets(p);
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
                        ("caption", &mut m.caption),
                        ("copyright", &mut m.copyright),
                        ("usageTerms", &mut m.usage_terms),
                        ("copyrightUrl", &mut m.copyright_url),
                        ("creator", &mut m.creator),
                        ("location", &mut m.location),
                        ("city", &mut m.city),
                        ("state", &mut m.state),
                        ("country", &mut m.country),
                        ("altText", &mut m.alt_text),
                        ("extendedDescription", &mut m.extended_description),
                    ] {
                        if let Some(v) = str_param(p, k) {
                            *field = v.to_string();
                        }
                    }
                    // keywords are stored cleaned (`a | b ` is `a|b`), each once whatever its case
                    use lightcraft_catalog::keywords::{clean, same};
                    let add = |m: &mut lightcraft_catalog::Meta, k: &str| {
                        let k = clean(k);
                        if !k.is_empty() && !m.keywords.iter().any(|x| same(x, &k)) {
                            m.keywords.push(k);
                        }
                    };
                    if let Some(k) = strs("keywords") {
                        m.keywords.clear();
                        for k in k {
                            add(&mut m, &k);
                        }
                    }
                    for k in strs("addKeywords").unwrap_or_default() {
                        add(&mut m, &k);
                    }
                    for k in strs("removeKeywords").unwrap_or_default() {
                        let k = clean(&k);
                        m.keywords.retain(|x| !same(&clean(x), &k));
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
        cmd!("album.create", "New Album", ["File"], None, "{name, parent?: folderId, folder?: bool, addSelected?: bool}", always, |s, p| {
            let name = str_param(p, "name").unwrap_or("Untitled Album").trim().to_string();
            if name.is_empty() {
                return Err(bad("album.create", "empty name"));
            }
            let folder = bool_or(p, "folder", false);
            let parent = p.get("parent").and_then(Value::as_u64).map(AlbumId);
            let photos = if !folder && bool_or(p, "addSelected", false) { s.targets(&Value::Null) } else { vec![] };
            let id = s.catalog.alloc_album_id();
            let cover = photos.first().copied();
            s.commit(
                if folder { "New Folder" } else { "New Album" },
                Op::AddAlbum { album: Album { id, name, parent, folder, photos, cover, smart: None, quick: false, order: None } },
            )?;
            Ok(json!({"id": id.0}))
        }),
        cmd!(
            "album.createSmart",
            "New Smart Album…",
            [],
            None,
            "{name, rules?: partial Filter (rating, ratingOp, flag, label, kind, edited, keyword, camera, lens, dateFrom, dateTo, date, text, album, ruleSet: {match: all|any|none, rules: [{field, op, value} | {group: ruleSet}]} — see album.ruleFields), parent?: folderId} — without `rules`, saves the current view (source + filter)",
            always,
            |s, p| {
                let name = str_param(p, "name").unwrap_or("Smart Album").trim().to_string();
                if name.is_empty() {
                    return Err(bad("album.createSmart", "empty name"));
                }
                let rules = match p.get("rules") {
                    Some(r) => merge_rules(&lightcraft_catalog::Filter::default(), r, "album.createSmart", &s.catalog, None)?,
                    None => view_rules(s),
                };
                let parent = p.get("parent").and_then(Value::as_u64).map(AlbumId);
                // the view of a folder, saved inside that folder, would be limited to itself
                if let Some(folder) = parent {
                    smart_loop_in(&s.catalog, &name, &rules, folder).map_or(Ok(()), |why| Err(bad("album.createSmart", why)))?;
                }
                let id = s.catalog.alloc_album_id();
                let album = Album { parent, smart: Some(Box::new(rules)), ..Album::new(id, name) };
                s.commit("New Smart Album", Op::AddAlbum { album })?;
                Ok(json!({"id": id.0, "count": s.catalog.album_count(id)}))
            }
        ),
        cmd!(query "album.ruleFields", "Smart Album Rule Fields", [], None, "{} → [{field, label, kind (text|keywords|number|date|choice|bool|album: an album id), group (field-menu submenu, null at the top level), ops: [{op, label}], choices? (ids), choiceLabels? ({id: label})}] for ruleSet rules, in menu order", always, |_, _| {
            use lightcraft_catalog::rules::{FIELDS, Kind, field_group, ops_for};
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
            "Edit Smart Album",
            [],
            None,
            "{id, rules?: partial Filter merged onto the current rules (null clears a field), replace?: bool, fromView?: bool (use the current view), name?: also rename it (blank keeps the name) — one undo step, both or neither}",
            always,
            |s, p| {
                let id = album_param(p, "id", "album.setRules")?;
                let cur = s.catalog.album(id).and_then(|a| a.smart.as_deref().cloned()).ok_or_else(|| bad("album.setRules", "not a smart album"))?;
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
                        .filter(|p| p.issue == lightcraft_catalog::rules::Issue::AlbumLoop)
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
                    // nor one for the album or folder of albums a saved view is limited to
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
                    Some(name) => s.commit("Edit Smart Album", Op::Batch { ops: vec![Op::RenameAlbum { id, name: name.to_string() }, set] })?,
                    None => s.commit("Edit Smart Album", set)?,
                };
                Ok(json!({"count": s.catalog.album_count(id)}))
            }
        ),
        cmd!("album.rename", "Rename Album", [], None, "{id, name}", always, |s, p| {
            let id = album_param(p, "id", "album.rename")?;
            let name = str_param(p, "name").ok_or_else(|| bad("album.rename", "missing name"))?.to_string();
            s.commit("Rename Album", Op::RenameAlbum { id, name })?;
            ok()
        }),
        cmd!(
            "album.delete",
            "Delete Album",
            [],
            None,
            "{id} — deletes the album or folder and its descendants, keeping photos in the library; one undo step",
            always,
            |s, p| {
                let id = album_param(p, "id", "album.delete")?;
                let folder = s.catalog.album(id).ok_or_else(|| bad("album.delete", "no such album"))?.folder;
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
                        return Err(bad("album.delete", "cycle in album folders"));
                    }
                    order.push(next);
                    if let Some(nested) = children.remove(&next) {
                        pending.extend(nested);
                    }
                }
                let ops = order.into_iter().rev().map(|id| Op::RemoveAlbum { id }).collect();
                // a deleted album that was being shown gives way to All Photos, as after any
                // other way it goes (`Session::reconcile_view`)
                s.commit(if folder { "Delete Folder" } else { "Delete Album" }, Op::Batch { ops })?;
                ok()
            }
        ),
        cmd!("album.move", "Move Album", [], None, "{id, parent?: folderId|null}", always, |s, p| {
            let id = album_param(p, "id", "album.move")?;
            let parent = p.get("parent").and_then(Value::as_u64).map(AlbumId);
            moved_loop_in(&s.catalog, id, parent).map_or(Ok(()), |why| Err(bad("album.move", why)))?;
            s.commit("Move Album", Op::MoveAlbum { id, parent })?;
            ok()
        }),
        cmd!(
            "album.reorder",
            "Reorder Album",
            [],
            None,
            "{id, parent?: folderId|null, before?: albumId|null} — places the album (or folder) in `parent` (default: where it is; null: the top level) before the sibling `before` of the same kind, or last of its kind; the folder is then ordered by hand. One undo step. → {changed}: how many edits it took (0: already there)",
            always,
            |s, p| {
                let id = album_param(p, "id", "album.reorder")?;
                let me = s.catalog.album(id).ok_or_else(|| bad("album.reorder", "no such album"))?;
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
                            return Err(bad("album.reorder", "folders are placed among folders and albums among albums"));
                        }
                        at
                    }
                    None if me.folder => order.iter().take_while(|x| s.catalog.album(**x).is_some_and(|a| a.folder)).count(),
                    None => order.len(),
                };
                order.insert(at.min(order.len()), id);
                let moved = me.parent != parent;
                if moved {
                    moved_loop_in(&s.catalog, id, parent).map_or(Ok(()), |why| Err(bad("album.reorder", why)))?;
                }
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
                s.commit("Reorder Album", Op::Batch { ops })?;
                Ok(json!({"changed": changed}))
            }
        ),
        cmd!(
            "album.sort",
            "Sort Albums by Name",
            [],
            None,
            "{parent?: folderId|null} — drops the hand order of the albums inside the folder (default: the top level), so they are listed by name → {changed}: how many albums lost their place",
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
                s.commit("Sort Albums by Name", Op::Batch { ops })?;
                Ok(json!({"changed": changed}))
            }
        ),
        cmd!("album.addPhotos", "Add to Album", ["Photo"], None, "{id: albumId, ids?: [photoIds]} (default: selection)", has_selection, |s, p| {
            let id = album_param(p, "id", "album.addPhotos")?;
            // photos that are there: an album doesn't list one that is gone (a selection dragged
            // here after an undo took one of its photos away)
            let mut targets = ids_param(p).unwrap_or_else(|| s.targets(&Value::Null));
            targets.retain(|t| s.catalog.photo(*t).is_some());
            let al = s.catalog.album(id).ok_or_else(|| bad("album.addPhotos", "no such album"))?;
            if al.is_smart() || al.folder {
                return Err(bad("album.addPhotos", "smart albums and folders can't hold photos"));
            }
            let mut photos = al.photos.clone();
            let before = photos.len();
            for t in targets {
                if !photos.contains(&t) {
                    photos.push(t);
                }
            }
            let added = photos.len() - before;
            let cover = al.cover.or(photos.first().copied());
            s.commit("Add to Album", Op::Batch { ops: vec![Op::SetAlbumPhotos { id, photos }, Op::SetAlbumCover { id, cover }] })?;
            Ok(json!({"added": added}))
        }),
        cmd!(
            "album.toggleTarget",
            "Add to Target Album",
            [],
            None,
            "{ids?} — B: adds the photos to the target album (the Quick Collection unless one is set), or removes them when they're all in it → {album, added}",
            has_selection,
            |s, p| {
                let targets = ids_param(p).unwrap_or_else(|| s.targets(&Value::Null));
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
                let al = s.catalog.album(id).ok_or_else(|| bad("album.toggleTarget", "no target album"))?;
                let all_in = !targets.is_empty() && targets.iter().all(|t| al.photos.contains(t));
                let photos: Vec<PhotoId> = if all_in {
                    al.photos.iter().copied().filter(|x| !targets.contains(x)).collect()
                } else {
                    al.photos.iter().copied().chain(targets.iter().copied().filter(|t| !al.photos.contains(t))).collect()
                };
                let name = al.name.clone();
                let cover = al.cover.filter(|c| photos.contains(c)).or(photos.first().copied());
                s.commit(
                    if all_in { "Remove from Target Album" } else { "Add to Target Album" },
                    Op::Batch { ops: vec![Op::SetAlbumPhotos { id, photos }, Op::SetAlbumCover { id, cover }] },
                )?;
                Ok(json!({"album": id.0, "name": name, "added": !all_in, "count": s.catalog.album_count(id)}))
            }
        ),
        cmd!("album.setTarget", "Set as Target Album", [], None, "{id: albumId | null} (null = the Quick Collection)", always, |s, p| {
            s.target_album = match p.get("id").and_then(Value::as_u64) {
                Some(a) => {
                    let id = AlbumId(a);
                    let al = s.catalog.album(id).ok_or_else(|| bad("album.setTarget", "no such album"))?;
                    if al.is_smart() || al.folder {
                        return Err(bad("album.setTarget", "smart albums and folders can't hold photos"));
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
            "library.autoImport",
            "Auto Import Settings",
            [],
            None,
            "{folder?: path | null (off), copy?: bool (copy into the library's Originals, else add in place), album?: name | null} — a watched folder whose new photos are added as they arrive (library.autoImportScan; the app scans every few seconds) → the settings",
            always,
            |s, p| {
                if let Some(f) = p.get("folder") {
                    s.import_defaults.auto_folder = match f.as_str().map(str::trim).filter(|f| !f.is_empty()) {
                        Some(f) if std::path::Path::new(f).is_dir() || cfg!(target_arch = "wasm32") => Some(f.to_string()),
                        Some(f) => return Err(bad("library.autoImport", format!("`{f}` is not a folder"))),
                        None => None,
                    };
                }
                if let Some(c) = p.get("copy").and_then(Value::as_bool) {
                    s.import_defaults.auto_copy = c;
                }
                if let Some(a) = p.get("album") {
                    s.import_defaults.auto_album = a.as_str().map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
                }
                s.save_prefs()?;
                let d = &s.import_defaults;
                Ok(json!({"folder": d.auto_folder, "copy": d.auto_copy, "album": d.auto_album}))
            }
        ),
        cmd!(
            "library.autoImportScan",
            "Auto Import Now",
            [],
            None,
            "{listing?: [[path, size]] (the folder as listed by the caller, e.g. on a worker thread), start?: bool (false: return the `library.import` params as `import` instead of importing)} — add the watched folder's new photos (files the library doesn't have yet; partial / still-copying files wait for the next scan) → {imported, folder, import?}",
            always,
            |s, p| {
                let Some(folder) = s.import_defaults.auto_folder.clone() else { return Ok(json!({"imported": [], "folder": null})) };
                // only files that stopped growing: a file still being written is left for later
                let known: std::collections::HashSet<String> = s
                    .catalog
                    .photos()
                    .filter_map(|p| match &p.source {
                        lightcraft_catalog::Source::File { path } => Some(path.clone()),
                        _ => None,
                    })
                    .collect();
                // the folder's files and sizes: listed here, or already listed on a worker thread (the app)
                let listing: Vec<(String, u64)> = match p.get("listing").and_then(Value::as_array) {
                    Some(l) => l.iter().filter_map(|e| Some((e.get(0)?.as_str()?.to_string(), e.get(1)?.as_u64()?))).collect(),
                    None => list_auto_import_folder(&folder).map_err(|e| bad("library.autoImportScan", e))?,
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
                let mut params = json!({"paths": fresh, "mode": mode});
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
                Ok(json!({"imported": r["imported"], "folder": folder}))
            }
        ),
        cmd!("album.removePhotos", "Remove from Album", [], None, "{id: albumId, ids?}", has_selection, |s, p| {
            let id = album_param(p, "id", "album.removePhotos")?;
            let targets = ids_param(p).unwrap_or_else(|| s.targets(&Value::Null));
            let al = s.catalog.album(id).ok_or_else(|| bad("album.removePhotos", "no such album"))?;
            if al.is_smart() {
                return Err(bad("album.removePhotos", "smart albums update automatically: change their rules"));
            }
            if al.folder {
                return Err(bad("album.removePhotos", "a folder shows the photos of the albums in it: remove them from those albums"));
            }
            let photos: Vec<PhotoId> = al.photos.iter().copied().filter(|x| !targets.contains(x)).collect();
            s.commit("Remove from Album", Op::SetAlbumPhotos { id, photos })?;
            ok()
        }),
        cmd!("album.setCover", "Set as Album Cover", [], None, "{id: albumId, photo?: photoId}", has_active, |s, p| {
            let id = album_param(p, "id", "album.setCover")?;
            if s.catalog.album(id).is_some_and(|a| a.folder) {
                return Err(bad("album.setCover", "a folder has no cover: set one on an album in it"));
            }
            let photo = p.get("photo").and_then(Value::as_u64).map(PhotoId).or(s.active());
            s.commit("Set Album Cover", Op::SetAlbumCover { id, cover: photo })?;
            ok()
        }),
        // ---- import
        cmd!(query "library.inspectLightroom", "Inspect Lightroom Catalog", [], None,
        "{path: .lrcat} → {photos, collections, missing, warnings}; read-only, no Lightroom required",
        always, |_, p| {
            let path = str_param(p,"path").ok_or_else(|| bad("library.inspectLightroom","missing path"))?;
            let data = crate::lightroom_catalog::read(std::path::Path::new(path)).map_err(|e| bad("library.inspectLightroom",e))?;
            Ok(json!({"photos":data.photos.len(),"collections":data.collections.iter().filter(|r|r.get("systemOnly").and_then(Value::as_f64).unwrap_or(0.0)==0.0).count(),"missing":data.photos.iter().filter(|p| !std::path::Path::new(&p.path).is_file()).map(|p|&p.path).collect::<Vec<_>>(),"warnings":data.warnings}))
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
            "{paths: [file or folder (recursive)]} → {candidates: [{path, name, format, kind, width, height, fileSize, captured, duplicate?: path|content, existing?, error?, previewOnly?: why a raw can only be shown from its embedded preview}], duplicates, scanned} — nothing is added",
            always,
            |s, p| {
                let paths = strs(p, "paths");
                if paths.is_empty() {
                    return Err(bad("library.importPreview", "no paths"));
                }
                let c = crate::import::scan(s, &paths);
                let dups = c.iter().filter(|c| c.duplicate.is_some()).count();
                Ok(json!({"scanned": c.len(), "duplicates": dups, "candidates": c}))
            }
        ),
        cmd!(
            "library.import",
            "Import Photos",
            ["File"],
            Some("Cmd+Shift+I"),
            "{paths: [file or folder (recursive)], mode?: add|copy|move (add = reference the files in place; copy = into the library's Originals/YYYY/YYYY-MM-DD/; move = as copy, then each original and its XMP sidecars are removed from the source — only after the copy is verified (a hard link on the same volume, else copied, synced and compared byte for byte) and its catalog record is saved; failed, duplicate and unchecked files keep their sources; a taken name gets -1, -2…; undo removes the photos from the library but leaves the files at the destination), destination?: folder for copies / moves, organize?: date (YYYY/YYYY-MM-DD) | month (YYYY/YYYY-MM) | flat | a folder template, e.g. `{date:%Y}/{date:%Y%m%d}` → 2026/20260114 (the template's `/` make the folders, each level expanded with the rename tokens and made a safe folder name: never outside the destination; must be relative, no `..`; a level with missing metadata is `unknown`) — dated by capture time, else the import time, rename?: file-name template for copies, original extension added (tokens: {name} {num} {seq} {seq:N} {date} {date:%Y%m%d} {folder} {camera} {lens} {iso} {rating} {title} {creator} {ext}; photo.renameTokens explains each; blank = keep names), renameStart?: 1, metadataPreset?: name, dng?: bool (copy raws as DNG; copy only), local?: bool (browsing: the photos stay out of the library, like library.browse; not with move), album?: albumId, albumName?: new album, preset?: presetId, keywords?: [..], onDeleted?: skip|restore|fresh (a file that is in Recently Deleted: skip = leave it there and say so, restore = bring the photo back with its edits, fresh = delete the trashed record and import the file as new)} → {imported, duplicates: [{path, existing, reason, existingDeleted?}], restored?: [photoId], failed, moved?: [{from, to, sidecars?}], kept?: [{path, reason}] (move: sources left in place and why), album?}",
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
        cmd!(query "library.info", "Library Info", [], None, "{}", always, |s, _| {
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
                },
            }))
        }),
        cmd!("library.compact", "Optimize Library", ["File"], None, "{}", has_library, |s, _| {
            s.compact_library()?;
            ok()
        }),
        cmd!("library.clearPreviews", "Clear Preview Cache", ["File"], None, "{}", always, |s, _| {
            s.media.rendered.clear();
            s.media.clear_sources();
            ok()
        }),
    ]
}

/// `base` with a partial Filter (JSON) merged on top.
/// `patch` merged onto `base`, its rules brought up to date (`RuleSet::upgrade`), unchecked (see
/// [`merge_rules`]).
fn merge_filter(base: &lightcraft_catalog::Filter, patch: &Value, c: &str) -> Result<lightcraft_catalog::Filter> {
    let mut v = serde_json::to_value(base).unwrap_or_default();
    lightcraft_develop::presets::deep_merge(&mut v, patch);
    let mut f: lightcraft_catalog::Filter = serde_json::from_value(v).map_err(|e| bad(c, e.to_string()))?;
    if let Some(rules) = f.rule_set.as_mut() {
        rules.upgrade();
    }
    Ok(f)
}

/// `patch` merged onto `base`, refused when the rule set it sets has problems (`RuleSet::check_for`;
/// `owner` is the smart album the rules are for, so testing an album that leads back to it is a
/// problem too).
fn merge_rules(
    base: &lightcraft_catalog::Filter,
    patch: &Value,
    c: &str,
    cat: &lightcraft_catalog::Catalog,
    owner: Option<AlbumId>,
) -> Result<lightcraft_catalog::Filter> {
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

/// Why a smart album called `name` with `rules` can't be in folder `parent`: its rules lead to
/// that folder or one around it (the view of a folder, saved inside it), so it would be one of
/// the albums it asks about. `None` when it can.
fn smart_loop_in(cat: &lightcraft_catalog::Catalog, name: &str, rules: &lightcraft_catalog::Filter, parent: AlbumId) -> Option<String> {
    cat.smart_album_would_loop_in(rules, parent).then(|| {
        let folder = cat.album(parent).map(|f| f.name.as_str()).unwrap_or_default();
        format!("smart album “{name}” would include itself in folder “{folder}”: its rules show that folder or one around it. Put it outside them")
    })
}

/// [`smart_loop_in`] for album or folder `id` on its way into `parent` (`None`: the top level,
/// where nothing loops): the first smart album among what moves that would include itself.
fn moved_loop_in(cat: &lightcraft_catalog::Catalog, id: AlbumId, parent: Option<AlbumId>) -> Option<String> {
    let parent = parent?;
    let looping = cat.album(cat.album_move_would_loop(id, parent)?)?;
    smart_loop_in(cat, &looping.name, looping.smart.as_deref()?, parent)
}

/// Whether `path` lies above a disk: some of its `ids` photos are on a disk (not the startup
/// disk) that the folder does not lie in. Such a folder (`/Volumes`, `/mnt`…) is not a folder of
/// one disk but a way to reach several.
pub(crate) fn covers_other_disks(s: &Session, path: &str, ids: &[PhotoId]) -> bool {
    ids.iter()
        .filter_map(|id| s.catalog.photo(*id))
        .filter_map(|p| lightcraft_catalog::folders::volume_of(p))
        .any(|v| v != "/" && !lightcraft_catalog::query::folder_within(path, &v))
}

/// The current view (source + filter) as smart-album rules. Viewing a smart album starts from
/// its rules with the filter bar's settings on top.
fn view_rules(s: &Session) -> lightcraft_catalog::Filter {
    use lightcraft_catalog::Filter;
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
    pub fn view_rules(&self) -> lightcraft_catalog::Filter {
        view_rules(self)
    }
}

fn has_library(s: &Session) -> std::result::Result<(), String> {
    if s.library.is_some() { Ok(()) } else { Err("no library is open (in-memory session)".into()) }
}

/// Coordinates typed by a person: "51.5072, -0.1276", "51.5072 N 0.1276 W",
/// "51°30'26\"N 0°7'39\"W" (degrees, minutes, seconds; N/S/E/W or signs).
pub(crate) fn parse_gps(t: &str) -> Option<(f64, f64)> {
    let t = t.trim();
    // split into the two coordinates: at a comma, else after the first N/S hemisphere letter
    let (a, b) = match t.split_once(',') {
        Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
        None => {
            let i = t.find(['N', 'S', 'n', 's']).map(|i| i + 1).or_else(|| t.find(char::is_whitespace))?;
            (t[..i].trim().to_string(), t[i..].trim().to_string())
        }
    };
    let one = |s: &str, pos: char, neg: char| -> Option<f64> {
        let up = s.to_ascii_uppercase();
        let sign = if up.contains(neg) || up.trim_start().starts_with('-') { -1.0 } else { 1.0 };
        let nums: Vec<f64> =
            up.split(|c: char| !(c.is_ascii_digit() || c == '.')).filter(|x| !x.is_empty()).map(|x| x.parse::<f64>().ok()).collect::<Option<_>>()?;
        let v = match nums.as_slice() {
            [d] => *d,
            [d, m] => d + m / 60.0,
            [d, m, s] => d + m / 60.0 + s / 3600.0,
            _ => return None,
        };
        let _ = pos;
        Some(sign * v)
    };
    let (la, lo) = (one(&a, 'N', 'S')?, one(&b, 'E', 'W')?);
    (la.abs() <= 90.0 && lo.abs() <= 180.0).then_some((la, lo))
}

#[cfg(test)]
mod gps_tests {
    #[test]
    fn coordinates_people_type() {
        let p = super::parse_gps;
        assert_eq!(p("51.5072, -0.1276"), Some((51.5072, -0.1276)));
        let (la, lo) = p("51°30'26\"N 0°7'39\"W").unwrap();
        assert!((la - 51.50722).abs() < 1e-4 && (lo + 0.1275).abs() < 1e-4, "{la} {lo}");
        let (la, lo) = p("33.8688 S, 151.2093 E").unwrap();
        assert!((la + 33.8688).abs() < 1e-9 && (lo - 151.2093).abs() < 1e-9);
        assert_eq!(p("95, 10"), None);
        assert_eq!(p("hello"), None);
    }
}

#[cfg(test)]
#[path = "library/tests_relative_ratings.rs"]
mod relative_rating_tests;
