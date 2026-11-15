//! Recognition commands: `faces.index` (embed the faces in the library), `faces.suggest` (who might an unnamed
//! face be?), `faces.person` (one person's faces, and the unnamed faces that look like them), `faces.unnamed` (every
//! unnamed face, look-alikes together) and `faces.setName` / `faces.nameFaces` (name one face, or many at once, which is
//! what teaches the suggestions). `faces.renamePerson` renames every in-library face of one person, or merges two
//! people when asked.
//!
//! Everything here is a suggestion for the user to confirm; nothing is ever named automatically. All of it is
//! catalog-only (no sidecar is written) and naming is undoable.

use lcc_catalog::Op;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd};
use crate::{Result, Session};

/// The `name` parameter of the naming commands: a name, or none (`null`, empty) to clear it.
fn name_param(p: &Value, c: &str) -> Result<Option<String>> {
    let name = match p.get("name") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or_else(|| bad(c, "`name` must be text or null"))?.trim().to_string()).filter(|n| !n.is_empty()),
    };
    if let Some(n) = &name
        && (n.chars().count() > 200 || n.chars().any(char::is_control))
    {
        return Err(bad(c, "a name is at most 200 characters, without control characters"));
    }
    Ok(name)
}

/// `faces.ignore`: ignore faces (or restore them, `ignored: false`) in one undo step. Every face is checked before
/// anything changes, so a stale selection changes nothing.
fn ignore_faces(s: &mut Session, p: &Value) -> Result<Value> {
    use lcc_meta::RegionKind;
    const C: &str = "faces.ignore";
    let ignored = match p.get("ignored") {
        None => true,
        Some(v) => v.as_bool().ok_or_else(|| bad(C, "`ignored` must be true or false"))?,
    };
    let faces = p.get("faces").and_then(Value::as_array).ok_or_else(|| bad(C, "missing `faces`"))?;
    if faces.is_empty() || faces.len() > 100_000 {
        return Err(bad(C, "select between 1 and 100000 faces"));
    }
    let mut edits: std::collections::BTreeMap<lcc_catalog::PhotoId, lcc_catalog::Meta> = Default::default();
    let mut seen = std::collections::HashSet::new();
    for face in faces {
        let id = lcc_catalog::PhotoId(face.get("photo").and_then(Value::as_u64).ok_or_else(|| bad(C, "missing `photo`"))?);
        let index = face.get("index").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(C, "invalid face `index`"))?;
        if !seen.insert((id, index)) {
            continue;
        }
        let photo = s.catalog.photo(id).filter(|p| p.in_library()).ok_or_else(|| bad(C, "photo is no longer in the catalog"))?;
        let region = photo.meta.regions.get(index).ok_or_else(|| bad(C, "face no longer exists"))?;
        let expected = if ignored { RegionKind::Face } else { RegionKind::IgnoredFace };
        if region.kind != expected {
            return Err(bad(C, "a selected face changed; reload the faces and try again"));
        }
        if let Some(rect) = face.get("rect") {
            let rect: lcc_geom::Rect = serde_json::from_value(rect.clone()).map_err(|_| bad(C, "invalid `rect`"))?;
            if rect != region.rect {
                return Err(bad(C, "a selected face changed; reload the faces and try again"));
            }
        }
        let meta = edits.entry(id).or_insert_with(|| photo.meta.clone());
        if let Some(region) = meta.regions.get_mut(index) {
            region.kind = if ignored { RegionKind::IgnoredFace } else { RegionKind::Face };
        }
    }
    let count = seen.len();
    s.commit(
        if ignored { "Ignore Faces" } else { "Restore Ignored Faces" },
        Op::Batch { ops: edits.into_iter().map(|(id, meta)| Op::SetMeta { id, meta: Box::new(meta) }).collect() },
    )?;
    // regions are not written to XMP, so a sidecar must not be rewritten for this
    s.skip_auto_write = true;
    Ok(json!({"changed": count, "ignored": ignored}))
}

/// `faces.ignored`: one page of the ignored faces, in catalog order (needs no recognition model).
fn ignored_faces(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.ignored";
    let offset = match p.get("offset") {
        None => 0,
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(C, "`offset` must be a non-negative index"))?,
    };
    let limit = match p.get("limit") {
        None => 1000,
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(C, "`limit` must be between 1 and 5000"))?,
    };
    if !(1..=5000).contains(&limit) {
        return Err(bad(C, "`limit` must be between 1 and 5000"));
    }
    let mut faces = Vec::new();
    let mut total = 0usize;
    for photo in s.catalog.photos().filter(|p| p.in_library()) {
        for (index, region) in photo.meta.regions.iter().enumerate().filter(|(_, r)| r.kind == lcc_meta::RegionKind::IgnoredFace) {
            if total >= offset && faces.len() < limit {
                faces.push(json!({"photo": photo.id.0, "index": index, "rect": region.rect, "name": region.name, "view": s.face_view(photo.id, region.rect)}));
            }
            total = total.saturating_add(1);
        }
    }
    Ok(json!({"total": total, "offset": offset, "faces": faces}))
}

/// Give a face region its name. Naming makes the face yours: a later "Detect Faces" run no longer replaces it.
fn apply_name(region: &mut lcc_meta::Region, name: &Option<String>) {
    region.name = name.clone();
    if name.is_some() && region.description.as_deref().is_some_and(|d| d.starts_with(super::face_detect::MARK)) {
        region.description = None;
    }
}

/// `faces.nameFaces {faces: [{photo, index}], name}`: name (or clear) many face regions at once, as one undo step: what
/// naming a group of look-alikes does.
fn name_faces(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.nameFaces";
    let name = name_param(p, C)?;
    let list = p.get("faces").and_then(Value::as_array).ok_or_else(|| bad(C, "missing `faces`"))?;
    if list.len() > 5000 {
        return Err(bad(C, "at most 5000 faces at a time"));
    }
    let mut by_photo: std::collections::BTreeMap<u64, Vec<usize>> = std::collections::BTreeMap::new();
    for f in list {
        let (Some(photo), Some(index)) =
            (f.get("photo").and_then(Value::as_u64), f.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()))
        else {
            return Err(bad(C, "each face needs a `photo` and an `index`"));
        };
        by_photo.entry(photo).or_default().push(index);
    }
    let (mut ops, mut named) = (Vec::new(), 0usize);
    for (photo, indexes) in by_photo {
        let id = lcc_catalog::PhotoId(photo);
        let Some(photo) = s.catalog.photo(id) else { continue };
        let mut meta = photo.meta.clone();
        let before = named;
        for i in indexes {
            if let Some(region) = meta.regions.get_mut(i).filter(|r| r.kind == lcc_meta::RegionKind::Face) {
                apply_name(region, &name);
                named += 1;
            }
        }
        if named > before {
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
        }
    }
    if ops.is_empty() {
        return Err(bad(C, "none of those faces exist"));
    }
    let photos = ops.len();
    s.commit(if name.is_some() { "Name Faces" } else { "Clear Face Names" }, Op::Batch { ops })?;
    s.skip_auto_write = true;
    Ok(json!({"named": named, "photos": photos, "name": name}))
}

/// `faces.setName {id?, index, name}`: name (or, with `null` or an empty name, un-name) one face region. Naming
/// makes the face yours: a later "Detect Faces" run no longer replaces it.
fn set_name(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.setName";
    let id = super::photo_id(p, C)?.or_else(|| s.active()).ok_or_else(|| bad(C, "no photo"))?;
    let index = p.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(C, "missing or invalid `index`"))?;
    let name = name_param(p, C)?;
    let mut meta = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?.meta.clone();
    let region = meta.regions.get_mut(index).ok_or_else(|| bad(C, "no such region"))?;
    apply_name(region, &name);
    s.commit(if name.is_some() { "Name Face" } else { "Clear Face Name" }, Op::SetMeta { id, meta: Box::new(meta) })?;
    s.skip_auto_write = true;
    Ok(json!({"id": id.0, "index": index, "name": name}))
}

/// `faces.renamePerson {from, to, merge?}`: give every in-library face of `from` the exact name `to`, as one undo
/// step. Names match the People view (trimmed, case-insensitive). A `to` that is already someone else is refused
/// unless `merge` is set, and then the two people become one. The sidecar is not touched, and a refusal changes nothing.
fn rename_person(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.renamePerson";
    let from = match p.get("from") {
        Some(Value::String(v)) => {
            let t = v.trim();
            if t.is_empty() {
                return Err(bad(C, "missing `from`"));
            }
            t.to_string()
        }
        Some(_) => return Err(bad(C, "`from` must be text")),
        None => return Err(bad(C, "missing `from`")),
    };
    let to = match p.get("to") {
        Some(Value::String(v)) => {
            let t = v.trim();
            if t.is_empty() {
                return Err(bad(C, "`to` must be a name"));
            }
            if t.chars().count() > 200 || t.chars().any(char::is_control) {
                return Err(bad(C, "a name is at most 200 characters, without control characters"));
            }
            t.to_string()
        }
        Some(_) => return Err(bad(C, "`to` must be text")),
        None => return Err(bad(C, "missing `to`")),
    };
    let merge = super::bool_or(p, "merge", false);
    let from_key = from.to_lowercase();
    let to_key = to.to_lowercase();
    let mut matched = 0usize;
    let mut other = false;
    let mut faces = 0usize;
    let mut ops = Vec::new();
    let ids: Vec<_> = s.catalog.photos().filter(|ph| ph.in_library()).map(|ph| ph.id).collect();
    for id in ids {
        let Some(ph) = s.catalog.photo(id) else { continue };
        let mut indexes = Vec::new();
        for (i, r) in ph.meta.regions.iter().enumerate() {
            if !matches!(r.kind, lcc_meta::RegionKind::Face | lcc_meta::RegionKind::IgnoredFace) {
                continue;
            }
            let Some(n) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
            let key = n.to_lowercase();
            if key == from_key {
                matched += 1;
                if r.name.as_deref() != Some(to.as_str()) {
                    indexes.push(i);
                }
            } else if key == to_key {
                other = true;
            }
        }
        if indexes.is_empty() {
            continue;
        }
        let mut meta = ph.meta.clone();
        for i in indexes {
            if let Some(region) = meta.regions.get_mut(i) {
                region.name = Some(to.clone());
                faces += 1;
            }
        }
        ops.push(Op::SetMeta { id, meta: Box::new(meta) });
    }
    if matched == 0 {
        return Err(bad(C, format!("no person named {from}")));
    }
    if faces == 0 {
        return Err(bad(C, format!("{to} is already their name")));
    }
    if other && !merge {
        return Err(bad(C, format!("{to} is already another person; pass merge: true to combine them")));
    }
    let photos = ops.len();
    s.commit(if other { "Merge People" } else { "Rename Person" }, Op::Batch { ops })?;
    s.skip_auto_write = true;
    if s.filter.person.as_deref().is_some_and(|n| n.trim().to_lowercase() == from_key) {
        s.filter.person = Some(to.clone());
    }
    if let Some(f) = s.shelved_filter.as_mut()
        && f.person.as_deref().is_some_and(|n| n.trim().to_lowercase() == from_key)
    {
        f.person = Some(to.clone());
    }
    Ok(json!({"faces": faces, "photos": photos, "from": from, "to": to, "merged": other}))
}

/// Photos to look in, from `scope`: absent, the whole library; `"filter"`, the photos the library filter shows (a year,
/// a folder, an album picked in the sidebar), any person filter aside, so People and a person's page follow the sidebar.
type Scope = Option<std::collections::HashSet<u64>>;

fn scope_of(s: &Session, cmd: &str, p: &Value) -> Result<Scope> {
    match p.get("scope") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) if v == "filter" => {
            let f = lcc_catalog::Filter { person: None, people: Vec::new(), ..s.filter.clone() };
            Ok(Some(s.catalog.query(&f, &lcc_catalog::Sort::default()).into_iter().map(|id| id.0).collect()))
        }
        Some(_) => Err(bad(cmd, "`scope` must be \"filter\" or left out")),
    }
}

fn in_scope(scope: &Scope, photo: u64) -> bool {
    scope.as_ref().is_none_or(|v| v.contains(&photo))
}

/// Most faces `faces.unnamed` lists.
const MAX_UNNAMED: usize = 3000;

/// How the unnamed faces were laid out.
struct Arranged {
    /// Faces that look alike are next to each other (only the ones already embedded can be).
    ordered: bool,
    /// A recognition model is chosen and its face index is available.
    ready: bool,
    /// A name for some faces, when the faces already named say so with confidence: `(photo, index)` to `(name, score)`.
    suggestions: std::collections::HashMap<(u64, usize), (String, f32)>,
    /// Look-alike groups (#259): faces next to each other in the order that are as alike as a match needs share a
    /// group number, so a frontend can select (and name or ignore) a group at once. `(photo, index)` to the group.
    groups: std::collections::HashMap<(u64, usize), usize>,
}

/// `faces.unnamed {limit?: 600}`: the faces nobody has named, to be named a group at a time. With a recognition model
/// chosen, faces that look alike are next to each other, and a face the named faces recognise carries the name they suggest.
fn unnamed(s: &mut Session, p: &Value) -> Result<Value> {
    let limit = p.get("limit").and_then(Value::as_u64).map_or(600, |n| usize::try_from(n).unwrap_or(600)).clamp(1, MAX_UNNAMED);
    let scope = scope_of(s, "faces.unnamed", p)?;
    let mut all: Vec<(u64, usize, lcc_geom::Rect)> = Vec::new();
    for ph in s.catalog.photos().filter(|ph| ph.in_library() && in_scope(&scope, ph.id.0)) {
        for (index, r) in ph.meta.regions.iter().enumerate() {
            if r.kind == lcc_meta::RegionKind::Face && r.name.is_none() {
                all.push((ph.id.0, index, r.rect));
            }
        }
    }
    all.sort_by_key(|(photo, index, _)| (*photo, *index));
    let total = all.len();
    let arranged = imp::arrange_unnamed(s, &mut all, limit);
    all.truncate(limit);
    let faces: Vec<Value> = all
        .iter()
        .map(|(photo, index, rect)| {
            let view = s.face_view(lcc_catalog::PhotoId(*photo), *rect);
            let suggestion = arranged.suggestions.get(&(*photo, *index)).map(|(name, score)| json!({"name": name, "score": score}));
            let group = arranged.groups.get(&(*photo, *index));
            json!({"photo": photo, "index": index, "rect": rect_json(rect), "view": rect_json(&view), "suggestion": suggestion, "group": group})
        })
        .collect();
    Ok(json!({"total": total, "faces": faces, "ordered": arranged.ordered, "ready": arranged.ready}))
}

/// What looking for faces that resemble a person found.
struct Similar {
    /// `{photo, index, rect, score}` of unnamed faces, the most alike first.
    items: Vec<serde_json::Value>,
    /// A recognition model is chosen and its face index is available, so an empty list means "none yet" rather than "not set up".
    ready: bool,
    /// Photos the background scan has still to do.
    pending: usize,
}

fn rect_json(r: &lcc_geom::Rect) -> Value {
    json!({"x0": r.x0, "y0": r.y0, "x1": r.x1, "y1": r.y1})
}

/// Most confirmed faces `faces.person` lists.
const MAX_CONFIRMED: usize = 5000;

/// `faces.person {name, more?}`: the faces named `name` (case-insensitively) across the library, and, when a recognition
/// model is chosen, the unnamed faces that look most like them, to be confirmed one by one.
fn person(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.person";
    let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "missing `name`"))?;
    let key = name.to_lowercase();
    let limit = p.get("more").and_then(Value::as_u64).map_or(120, |n| usize::try_from(n).unwrap_or(120)).min(1000);
    let scope = scope_of(s, C, p)?;
    let (mut shown, mut total, mut confirmed) = (name.to_string(), 0usize, Vec::new());
    for ph in s.catalog.photos().filter(|ph| ph.in_library()) {
        for (index, r) in ph.meta.regions.iter().enumerate() {
            let Some(n) = r.name.as_deref().filter(|n| r.kind == lcc_meta::RegionKind::Face && n.trim().to_lowercase() == key) else {
                continue;
            };
            if total == 0 {
                shown = n.trim().to_string();
            }
            total += 1;
            // the count is the whole library's; the faces shown follow the scope
            if confirmed.len() < MAX_CONFIRMED && in_scope(&scope, ph.id.0) {
                confirmed.push(json!({"photo": ph.id.0, "index": index, "rect": rect_json(&r.rect), "view": rect_json(&s.face_view(ph.id, r.rect))}));
            }
        }
    }
    let similar = if total > 0 { imp::similar(s, &key, limit, &scope) } else { Similar { items: Vec::new(), ready: false, pending: 0 } };
    Ok(
        json!({"name": shown, "total": total, "confirmed": confirmed, "more": similar.items, "ready": similar.ready, "pendingPhotos": similar.pending}),
    )
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use lcc_catalog::{Op, Photo, PhotoId};
    use lcc_develop::DevelopSettings;
    use lcc_faces::matching;
    use lcc_faces::runtime::Embedder;
    use lcc_geom::Rect;
    use lcc_meta::{Region, RegionKind};
    use rayon::prelude::*;
    use serde_json::{Value, json};

    use super::super::bad;
    use super::super::face_detect::{LABEL, MARK};
    use super::super::face_models::{finish_downloads, installed_models, read_settings, set_paused, switches, top_faces_value};
    use super::Similar;
    use crate::faces_worker::{Done, EDGE, PREFETCH, Pace, Prepared, Worker, max_workers, process, target_workers};
    use crate::{Result, Session};

    /// Defaults for suggestions, until a model's own threshold is known.
    const DEFAULT_THRESHOLD: f32 = 0.45;
    const DEFAULT_MARGIN: f32 = 0.06;
    /// How long a look at the models folder is trusted by the per-frame `faces.pump`.
    const RECHECK: Duration = Duration::from_secs(1);
    /// How often the background pump writes new embeddings to the cache file (appending is cheap; this keeps it to a few
    /// writes a minute, and a session that ends writes the rest).
    const SAVE_EVERY: Duration = Duration::from_secs(3);

    /// The chosen recognition model's manifest, with the face index reset to it when it changed, without loading the
    /// ONNX network. `fresh` looks at the models folder again; otherwise a look made in the last second is trusted.
    fn current_manifest(s: &mut Session, cmd: &str, fresh: bool) -> Result<lcc_faces::ModelManifest> {
        if !fresh && s.faces.checked.is_some_and(|t| t.elapsed() < RECHECK) {
            return s
                .faces
                .manifest
                .as_ref()
                .map(|(_, m)| m.clone())
                .ok_or_else(|| bad(cmd, "no recognition model is chosen: add one in Settings > Faces and press Use"));
        }
        s.faces.checked = Some(Instant::now());
        let dir = s.face_models_dir.clone().ok_or_else(|| {
            s.faces.manifest = None;
            bad(cmd, "this build has nowhere to keep face models")
        })?;
        let id = read_settings(&dir).embedder.ok_or_else(|| {
            s.faces.manifest = None;
            bad(cmd, "no recognition model is chosen: add one in Settings > Faces and press Use")
        })?;
        let installed = installed_models(&dir).into_iter().find(|i| i.manifest.id == id).ok_or_else(|| {
            s.faces.manifest = None;
            bad(cmd, "the chosen recognition model is not installed")
        })?;
        let dim = match installed.manifest.output {
            lcc_faces::OutputSpec::Embedding { dim } => dim as usize,
            _ => {
                s.faces.manifest = None;
                return Err(bad(cmd, "the chosen model is not a recognition model"));
            }
        };
        let mirror = if read_settings(&dir).mirror_average() { crate::faces_index::MIRROR_TAG } else { "" };
        let tag = format!("{id}@{}{mirror}", installed.manifest.sha256.as_deref().unwrap_or("unknown"));
        // which photos the detector has searched is kept beside the library (or in memory when it is not on disk)
        let scan_path = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("face-scanned.bin"));
        if s.faces.scanned.path() != scan_path.as_deref() {
            s.faces.scanned.reset(scan_path);
        }
        let cache = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(format!("face-embeddings-{id}.bin")));
        if s.faces.index.tag != tag || s.faces.index.path != cache {
            s.faces.index.reset(&tag, dim, cache);
            s.faces.queue.clear();
            s.faces.queue_stamp = None;
            s.faces.in_flight.clear();
        }
        s.faces.manifest = Some((tag, installed.manifest.clone()));
        Ok(installed.manifest)
    }

    /// The chosen recognition model, loaded, with the index reset to it when it changed. `fresh` looks at the models
    /// folder again; otherwise a look made in the last second is trusted (the UI asks every frame).
    fn current_with(s: &mut Session, cmd: &str, fresh: bool) -> Result<Arc<Embedder>> {
        let manifest = current_manifest(s, cmd, fresh)?;
        let tag = s.faces.index.tag.clone();
        if let Some((have, e)) = &s.faces.embedder
            && *have == tag
        {
            return Ok(e.clone());
        }
        let dir = s.face_models_dir.clone().ok_or_else(|| bad(cmd, "this build has nowhere to keep face models"))?;
        let embedder = Arc::new(Embedder::load(&dir.join(&manifest.id).join("model.onnx"), &manifest).map_err(|e| bad(cmd, e.to_string()))?);
        let net = embedder.clone();
        super::super::face_detect::offer_gpu(s, "recognition", move |gpu| net.accelerate(gpu, true).map_err(|e| e.to_string()));
        s.faces.embedder = Some((tag, embedder.clone()));
        Ok(embedder)
    }

    fn current(s: &mut Session, cmd: &str) -> Result<Arc<Embedder>> {
        current_with(s, cmd, true)
    }

    /// Whether the background scan should look for faces in this photo: it is in the library, has no face regions at all
    /// (nothing from XMP, nothing drawn or named, no earlier detection) and has not been searched yet.
    fn needs_search(s: &Session, photo: &Photo) -> bool {
        photo.in_library()
            && !photo.meta.regions.iter().any(|r| matches!(r.kind, RegionKind::Face | RegionKind::IgnoredFace))
            && !s.faces.scanned.contains(photo.id.0)
            && !s.faces.scan_failed.contains(&photo.id.0)
    }

    /// The photo's faces that still need embedding (and, with `search`, the faces to find in it first), with the render job
    /// for its picture. `None` when there is nothing to do.
    fn prepare(s: &mut Session, id: PhotoId, search: bool) -> Option<Prepared> {
        let tag = s.faces.index.tag.clone();
        let photo = s.catalog.photo(id)?.clone();
        let todo: Vec<Rect> =
            photo.meta.regions.iter().filter(|r| r.kind == RegionKind::Face && s.faces.index.needs(id.0, &r.rect)).map(|r| r.rect).collect();
        // finding faces needs the detector the user may not have downloaded: such a photo is left unsearched, not marked as searched
        let detector = crate::cmd::face_detect::detector(s).ok();
        let detect = search && detector.is_some() && needs_search(s, &photo);
        if todo.is_empty() && !detect {
            return None;
        }
        let job = s.preview_job(id, EDGE, EDGE, false, &DevelopSettings::default())?;
        let preview = s.scan_preview_of(&photo);
        Some(Prepared { tag, epoch: s.faces.epoch, id, content: crate::media::content_key(&photo), job, preview, todo, detect, detector })
    }

    /// The faces the detector found in a photo that had none become its face regions: unnamed, marked as detected (so a
    /// manual Detect Faces replaces them), and not an undo step, since the user did not do this.
    fn add_found(s: &mut Session, id: PhotoId, found: &[Rect]) {
        if found.is_empty() {
            return;
        }
        let Some(photo) = s.catalog.photo(id) else { return };
        // the user (or an import) got there first: leave their faces alone
        if photo.meta.regions.iter().any(|r| matches!(r.kind, RegionKind::Face | RegionKind::IgnoredFace)) {
            return;
        }
        let mut meta = photo.meta.clone();
        for rect in found {
            meta.regions.push(Region { rect: *rect, kind: RegionKind::Face, name: None, description: Some(format!("{MARK}{LABEL}")) });
        }
        if s.apply_system(Op::SetMeta { id, meta: Box::new(meta) }).is_ok() {
            // regions are not written to XMP, so no sidecar may be rewritten for this
            s.skip_auto_write = true;
        }
    }

    /// Take a finished photo into the index (ignored when it was made for another model or another library, or from
    /// a file the photo no longer has: issue #1524). Returns (embedded, failed).
    fn ingest(s: &mut Session, d: Done) -> (usize, usize) {
        s.faces.in_flight.remove(&d.id);
        if d.epoch != s.faces.epoch || d.tag != s.faces.index.tag {
            return (0, 0);
        }
        if s.catalog.photo(d.id).is_none_or(|p| crate::media::content_key(p) != d.content) {
            return (0, 0);
        }
        if d.detect_wanted {
            match &d.found {
                Some(found) => {
                    s.faces.scanned.insert(d.id.0);
                    add_found(s, d.id, found);
                }
                None => {
                    s.faces.scan_failed.insert(d.id.0);
                }
            }
        }
        let (mut embedded, mut failed) = (0, 0);
        for (rect, v, view) in d.results {
            match v {
                Some(v) if v.len() == s.faces.index.dim => {
                    s.faces.index.insert_with_view(d.id.0, &rect, v, &view);
                    embedded += 1;
                }
                _ => {
                    s.faces.index.skip(d.id.0, &rect);
                    failed += 1;
                }
            }
        }
        (embedded, failed)
    }

    /// Take in whatever the worker has finished.
    fn collect(s: &mut Session) -> (usize, usize) {
        let finished = s.faces.worker.as_ref().map(Worker::finished).unwrap_or_default();
        finished.into_iter().fold((0, 0), |(e, f), d| {
            let (de, df) = ingest(s, d);
            (e + de, f + df)
        })
    }

    /// Embed every face region of `id` the index does not have yet, here and now. Returns (embedded, failed).
    fn embed_photo(s: &mut Session, id: PhotoId, e: &Embedder) -> Result<(usize, usize)> {
        let Some(job) = prepare(s, id, false) else { return Ok((0, 0)) };
        Ok(ingest(s, process(job, e)))
    }

    /// Photos with a face that needs embedding (and, with `search`, photos to look for faces in): those the caller asked
    /// for first, then those with named faces (they are the gallery everything else is compared with), then the rest, and
    /// last the photos that only need searching.
    fn pending(s: &Session, first: &[PhotoId], search: bool) -> Vec<PhotoId> {
        let mut out: Vec<(u8, PhotoId)> = Vec::new();
        for p in s.catalog.photos().filter(|p| p.in_library()) {
            let faces: Vec<_> = p.meta.regions.iter().filter(|r| r.kind == RegionKind::Face).collect();
            let embed = faces.iter().any(|r| s.faces.index.needs(p.id.0, &r.rect));
            if embed || (search && needs_search(s, p)) {
                let rank = if first.contains(&p.id) {
                    0
                } else if faces.iter().any(|r| r.name.is_some()) {
                    1
                } else if embed {
                    2
                } else {
                    3
                };
                out.push((rank, p.id));
            }
        }
        out.sort_by_key(|(rank, id)| (*rank, id.0));
        out.into_iter().map(|(_, id)| id).collect()
    }

    pub fn index(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.index";
        let embedder = current(s, C)?;
        collect(s);
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(200).min(600_000));
        let first: Vec<PhotoId> = super::super::photo_ids(p, C)?.unwrap_or_default();
        let todo = pending(s, &first, false);
        let started = Instant::now();
        let (mut embedded, mut failed, mut done) = (0, 0, 0);
        for id in &todo {
            // at least one photo per call, so a slow model still makes progress; none when only the status is asked for
            if budget.is_zero() || (done > 0 && started.elapsed() >= budget) {
                break;
            }
            let (e, f) = embed_photo(s, *id, &embedder)?;
            (embedded, failed, done) = (embedded + e, failed + f, done + 1);
        }
        if let Err(e) = s.faces.index.save() {
            log::warn!("could not save the face embeddings: {e}");
        }
        Ok(json!({
            "model": s.faces.index.tag,
            "embedded": embedded,
            "failed": failed,
            "photosDone": done,
            "pendingPhotos": todo.len().saturating_sub(done),
            "indexedFaces": s.faces.index.len(),
            "ms": started.elapsed().as_secs_f64() * 1000.0,
        }))
    }

    fn str_scope(p: &Value) -> Option<&str> {
        p.get("scope").and_then(Value::as_str)
    }

    /// Seconds since 1970 of an ISO capture time (`2024-09-20T10:48:45…`, any zone suffix ignored): only for telling
    /// shots taken close together, so the zone does not matter.
    fn capture_secs(iso: &str) -> Option<i64> {
        let n = |a: usize, b: usize| iso.get(a..b)?.parse::<i64>().ok();
        let (y, m, d, hh, mm, ss) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        // Days since 1970-01-01 (proleptic Gregorian); a day past the month's length rolls over as
        // the old Hinnant arithmetic did.
        let first = chrono::NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, u32::try_from(m).ok()?, 1)?;
        let date = first.checked_add_days(chrono::Days::new(u64::try_from(d - 1).ok()?))?;
        let days = (date - chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?).num_days();
        Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
    }

    /// `faces.evaluate`: leave-one-out over the library's named faces, to see how well recognition works on *your*
    /// photos and which threshold to trust. Each named face in turn is treated as unknown and ranked against the
    /// other named faces, ignoring those from shots taken within `burstSecs` of it (near-duplicates would make
    /// everything look easy). Reports counts only.
    pub fn evaluate(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.evaluate";
        let embedder = current(s, C)?;
        collect(s);
        let burst = p.get("burstSecs").and_then(Value::as_i64).unwrap_or(5).clamp(0, 86_400 * 7);
        let margin = p.get("margin").and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(DEFAULT_MARGIN, |v| v as f32);
        let thresholds: Vec<f32> = p
            .get("thresholds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as f32).take(64).collect())
            .filter(|v: &Vec<f32>| !v.is_empty())
            .unwrap_or_else(|| (4..=16).map(|i| i as f32 * 0.05).collect());
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(0).min(3_600_000));
        let top = top_faces_param(s, C, p)?;
        let started = Instant::now();
        // `scope: "visible"` looks only at the photos the current filter shows (say one year, or one album)
        let scope: Option<std::collections::HashSet<PhotoId>> = (str_scope(p) == Some("visible")).then(|| s.visible_cloned().into_iter().collect());
        let in_scope = |id: PhotoId| scope.as_ref().is_none_or(|v| v.contains(&id));
        let todo: Vec<PhotoId> = pending(s, &[], false).into_iter().filter(|id| in_scope(*id)).collect();
        let mut done = 0;
        for id in &todo {
            if started.elapsed() >= budget {
                break;
            }
            embed_photo(s, *id, &embedder)?;
            done += 1;
        }
        // every named face with its embedding, the photo it is in, and when that was taken
        struct Named {
            name: String,
            emb: Arc<[f32]>,
            photo: PhotoId,
            when: Option<i64>,
        }
        let named: Vec<Named> = s
            .catalog
            .photos()
            .filter(|ph| ph.in_library() && in_scope(ph.id))
            .flat_map(|ph| {
                let when = ph.captured.as_deref().and_then(capture_secs);
                ph.meta
                    .regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Face)
                    .filter_map(|r| Some(Named { name: r.name.clone()?, emb: s.faces.index.get(ph.id.0, &r.rect)?.clone(), photo: ph.id, when }))
                    .collect::<Vec<_>>()
            })
            .collect();
        let near = |a: &Named, b: &Named| a.photo == b.photo || matches!((a.when, b.when), (Some(x), Some(y)) if (x - y).abs() <= burst);
        let mut sweep: Vec<(f32, usize, usize)> = thresholds.iter().map(|t| (*t, 0, 0)).collect();
        let (mut queries, mut top1, mut unmatchable) = (0usize, 0usize, 0usize);
        for q in &named {
            let others: Vec<&Named> = named.iter().filter(|g| !near(q, g)).collect();
            if !others.iter().any(|g| g.name.eq_ignore_ascii_case(&q.name)) {
                unmatchable += 1;
                continue;
            }
            let refs: Vec<(&str, &[f32])> = others.iter().map(|g| (g.name.as_str(), &g.emb[..])).collect();
            let ranked = matching::rank_top(&q.emb, &refs, top);
            queries += 1;
            if ranked.first().is_some_and(|r| r.name.eq_ignore_ascii_case(&q.name)) {
                top1 += 1;
            }
            for (t, suggested, correct) in sweep.iter_mut() {
                if let Some(sg) = matching::suggest(&ranked, *t, margin) {
                    *suggested += 1;
                    if sg.name.eq_ignore_ascii_case(&q.name) {
                        *correct += 1;
                    }
                }
            }
        }
        let pct = |n: usize, d: usize| if d == 0 { Value::Null } else { json!((n as f64 / d as f64 * 1000.0).round() / 1000.0) };
        Ok(json!({
            "model": s.faces.index.tag,
            "namedFaces": named.len(),
            "queries": queries,
            "unmatchable": unmatchable,
            "pendingPhotos": todo.len().saturating_sub(done),
            "top1": pct(top1, queries),
            "margin": margin,
            "topFaces": top,
            "burstSecs": burst,
            "sweep": sweep.iter().map(|(t, sg, ok)| json!({"threshold": t, "suggested": sg, "correct": ok, "precision": pct(*ok, *sg), "recall": pct(*ok, queries)})).collect::<Vec<_>>(),
        }))
    }

    /// The scan's row in the activity stack: there while `pending` photos are left (also while paused), its whole the
    /// most that were left at once since the scan last finished (which it returns; 0 when none are left). It has no ✕:
    /// its pause button pauses it (resume while `paused`), and its gear opens Settings ▸ Faces, where it is switched off.
    fn track(s: &mut Session, pending: u64, paused: bool) -> u64 {
        if pending == 0 {
            s.faces.task = None;
            s.faces.peak = 0;
            return 0;
        }
        s.faces.peak = s.faces.peak.max(pending);
        let task = s.faces.task.get_or_insert_with(|| s.activity.start("faces", "Finding faces", crate::activity::Cancel::No));
        task.progress(s.faces.peak.saturating_sub(pending), s.faces.peak);
        task.detail(if paused { "Paused" } else { "" });
        task.set_controls(Some(crate::activity::Controls {
            paused,
            pause: "faces.pause",
            resume: "faces.resume",
            settings_tab: "faces",
            settings_section: None,
        }));
        s.faces.peak
    }

    /// Whether recognition is on and the scan paused, looked up at most once a second.
    fn switches_cached(s: &mut Session) -> (bool, bool) {
        let Some(dir) = s.face_models_dir.clone() else { return (false, false) };
        if let Some((t, on, paused)) = s.faces.switches_seen
            && t.elapsed() < RECHECK
        {
            return (on, paused);
        }
        let (on, paused) = switches(&dir);
        s.faces.switches_seen = Some((Instant::now(), on, paused));
        (on, paused)
    }

    /// Where the scan stands: `off` (recognition switched off), `paused`, `indexing` (photos left), `complete` (none
    /// left) or `idle` (on, but not started yet: no model chosen, or not looked at yet).
    fn state(s: &mut Session) -> (&'static str, bool, bool) {
        let (on, paused) = switches_cached(s);
        let pending = s.faces.queue.len() + s.faces.in_flight.len();
        let state = if !on {
            "off"
        } else if paused {
            "paused"
        } else if pending > 0 {
            "indexing"
        } else if s.faces.queue_stamp.is_some() {
            "complete"
        } else {
            "idle"
        };
        (state, on, paused)
    }

    /// `faces.status`: how far the background face scan is.
    pub fn status(s: &mut Session, _: &Value) -> Result<Value> {
        let (state, enabled, paused) = state(s);
        let pending = (s.faces.queue.len() + s.faces.in_flight.len()) as u64;
        let total = s.faces.peak.max(pending);
        Ok(json!({
            "enabled": enabled,
            "paused": paused,
            "state": state,
            "done": total.saturating_sub(pending),
            "total": total,
            "pendingPhotos": pending,
            "indexedFaces": s.faces.index.len(),
            "searchedPhotos": s.faces.scanned.len(),
        }))
    }

    /// Drop what the scan has learned for this library: every face embedding (of every recognition model) and the list
    /// of photos already searched, in memory and on disk. Faces already in the catalog (named, or found by an earlier
    /// scan) stay. What a worker is still making is dropped when it comes back.
    fn drop_index(s: &mut Session, cmd: &str) {
        // the chosen model's index is loaded first, so its file is the one written fresh (none chosen: only the files go)
        let _ = current_manifest(s, cmd, true);
        s.faces.epoch = s.faces.epoch.wrapping_add(1);
        s.faces.index.clear();
        s.faces.scanned.clear();
        s.faces.scan_failed.clear();
        s.faces.queue.clear();
        s.faces.queue_stamp = None;
        s.faces.in_flight.clear();
        s.faces.task = None;
        s.faces.peak = 0;
        if let Some(dir) = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone())
            && let Ok(entries) = std::fs::read_dir(&dir)
        {
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name == "face-scanned.bin" || (name.starts_with("face-embeddings-") && name.ends_with(".bin")) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        // the chosen model's file and the searched list come back empty, so what is learned next is appended to them
        if let Err(e) = s.faces.index.save() {
            log::warn!("could not save the face embeddings: {e}");
        }
        if let Err(e) = s.faces.scanned.save() {
            log::warn!("could not save the list of searched photos: {e}");
        }
    }

    /// `faces.clearIndex`: drop the face index (see [`drop_index`]) and pause the scan, so it does not start over at once.
    pub fn clear(s: &mut Session, _: &Value) -> Result<Value> {
        const C: &str = "faces.clearIndex";
        set_paused(s, C, true)?;
        drop_index(s, C);
        status(s, &Value::Null)
    }

    /// `faces.restart`: drop the face index and scan the library again from the start.
    pub fn restart(s: &mut Session, _: &Value) -> Result<Value> {
        const C: &str = "faces.restart";
        set_paused(s, C, false)?;
        drop_index(s, C);
        status(s, &Value::Null)
    }

    /// `faces.pump`: cheap to call every frame. Takes in what the background worker has finished and, when it is idle,
    /// hands it the next photo with faces to embed. Does nothing (and says so) until recognition is switched on in
    /// Settings with a model chosen.
    pub fn pump(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.pump";
        // how hard the scan may work now, from what the user is doing (the app says); nothing new is started while paused,
        // and what is already running finishes
        // paused by the user (`faces.pause`): nothing new is started until `faces.resume`
        let (enabled, paused) = switches_cached(s);
        let pace = if paused { Pace::Pause } else { Pace::parse(p.get("pace").and_then(Value::as_str)) };
        let target = target_workers(pace);
        // a model that has finished downloading is installed and chosen here, whether or not recognition was on
        finish_downloads(s);
        if !enabled {
            track(s, 0, false);
            if let Some(w) = &s.faces.worker {
                w.allow(0, Pace::Pause);
            }
            let _ = current_manifest(s, C, false);
            collect(s);
            if (s.faces.index.has_unsaved() || s.faces.scanned.has_unsaved()) && s.faces.saved_at.is_none_or(|t| t.elapsed() >= SAVE_EVERY) {
                if let Err(e) = s.faces.index.save() {
                    log::warn!("could not save the face embeddings: {e}");
                }
                if let Err(e) = s.faces.scanned.save() {
                    log::warn!("could not save the list of searched photos: {e}");
                }
                s.faces.saved_at = Some(Instant::now());
            }
            return Ok(json!({
                "active": false,
                "paused": paused,
                "state": "off",
                "inFlight": 0,
                "pendingPhotos": 0,
                "peak": 0,
                "indexedFaces": s.faces.index.len(),
            }));
        }
        if s.faces.retry_at.is_some_and(|t| Instant::now() < t) {
            track(s, 0, paused);
            return Ok(json!({"active": false, "paused": paused, "state": "idle"}));
        }
        let Ok(embedder) = current_with(s, C, false) else {
            // the chosen model is gone or broken: look again in a few seconds, not every frame
            s.faces.retry_at = Some(Instant::now() + Duration::from_secs(3));
            track(s, 0, paused);
            return Ok(json!({"active": false, "paused": paused, "state": "idle"}));
        };
        s.faces.retry_at = None;
        // the card the model ran on stopped working mid-scan: the rest goes to the other card, or the CPU
        {
            let net = embedder.clone();
            super::super::face_detect::move_off_lost_card(s, "recognition", embedder.net().accelerator().is_some(), move |gpu| {
                net.accelerate(gpu, true).map_err(|e| e.to_string())
            });
        }
        let (embedded, _) = collect(s);
        if (s.faces.index.has_unsaved() || s.faces.scanned.has_unsaved()) && s.faces.saved_at.is_none_or(|t| t.elapsed() >= SAVE_EVERY) {
            if let Err(e) = s.faces.index.save() {
                log::warn!("could not save the face embeddings: {e}");
            }
            if let Err(e) = s.faces.scanned.save() {
                log::warn!("could not save the list of searched photos: {e}");
            }
            s.faces.saved_at = Some(Instant::now());
        }
        if s.faces.in_flight.is_empty() && s.faces.queue.is_empty() && s.faces.queue_stamp != Some(s.catalog.revision) {
            // the queue is taken from the end, so it is made last first
            let mut queue = pending(s, &[], true);
            queue.reverse();
            s.faces.queue = queue;
            s.faces.queue_stamp = Some(s.catalog.revision);
        }
        if target > 0 && s.faces.worker.is_none() && !s.faces.queue.is_empty() {
            s.faces.worker = Worker::start(max_workers());
        }
        // the workers are allowed `target` photos at once; a couple more wait behind them, so a worker that finishes has its
        // next photo at once instead of waiting for the next frame to hand it one
        if let Some(w) = &s.faces.worker {
            w.allow(target, pace);
        }
        // the GPU is held back the same way: a light pace leaves it mostly free for the window's own renders
        let (runs, share, second_share) = crate::faces_worker::gpu_pace(pace);
        lcc_gpu::faces::set_pace(runs, share, second_share);
        while target > 0 && s.faces.in_flight.len() < target + PREFETCH {
            let Some(id) = s.faces.queue.pop() else { break };
            let Some(job) = prepare(s, id, true) else { continue };
            if s.faces.worker.as_ref().is_some_and(|w| w.submit(embedder.clone(), job)) {
                s.faces.in_flight.insert(id);
            } else {
                break;
            }
        }
        let pending = s.faces.queue.len() + s.faces.in_flight.len();
        let peak = track(s, pending as u64, paused);
        let (state, _, _) = state(s);
        Ok(json!({
            "active": true,
            "paused": paused,
            "state": state,
            "inFlight": s.faces.in_flight.len(),
            "pendingPhotos": pending,
            "peak": peak,
            "indexedFaces": s.faces.index.len(),
            "searchedPhotos": s.faces.scanned.len(),
            "workers": target,
            "embedded": embedded,
        }))
    }

    /// The unnamed faces that look most like the person whose lower-cased name is `key`, best first, at most `limit`: each
    /// is scored like a suggestion (the mean of its two closest faces of theirs) and kept above four fifths of the
    /// model's suggestion bar, since a person looks at these before anything is named. Only faces already embedded can be
    /// found; `ready` says whether a recognition model is chosen and its index is available.
    pub fn similar(s: &mut Session, key: &str, limit: usize, scope: &super::Scope) -> Similar {
        let none = Similar { items: Vec::new(), ready: false, pending: 0 };
        let Some(_) = s.face_models_dir.clone() else { return none };
        let Ok(manifest) = current_manifest(s, "faces.person", false) else { return none };
        let pending = s.faces.queue.len() + s.faces.in_flight.len();
        let (mut gallery, mut candidates): (Vec<Arc<[f32]>>, Vec<(u64, usize, Rect, Arc<[f32]>)>) = (Vec::new(), Vec::new());
        for ph in s.catalog.photos().filter(|ph| ph.in_library()) {
            for (index, r) in ph.meta.regions.iter().enumerate().filter(|(_, r)| r.kind == RegionKind::Face) {
                let Some(e) = s.faces.index.get(ph.id.0, &r.rect) else { continue };
                match r.name.as_deref() {
                    Some(n) if n.trim().to_lowercase() == key => gallery.push(e.clone()),
                    Some(_) => {}
                    // their faces anywhere teach the match; only faces in scope are offered
                    None if super::in_scope(scope, ph.id.0) => candidates.push((ph.id.0, index, r.rect, e.clone())),
                    None => {}
                }
            }
        }
        let ready = Similar { items: Vec::new(), ready: true, pending };
        if gallery.is_empty() {
            return ready;
        }
        let bar = manifest.thresholds.match_cosine.unwrap_or(DEFAULT_THRESHOLD) * 0.8;
        let refs: Vec<&[f32]> = gallery.iter().map(|e| &e[..]).collect();
        let top = top_faces(s);
        // every unnamed face against every face of theirs: hundreds of millions of comparisons in a large
        // library, so on every core (minutes on one)
        let mut scored: Vec<(f32, u64, usize, Rect)> = candidates
            .into_par_iter()
            .filter_map(|(photo, index, rect, e)| {
                let score = matching::person_score(&e, &refs, top)?;
                (score >= bar).then_some((score, photo, index, rect))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        scored.truncate(limit);
        let items = scored
            .into_iter()
            .map(|(score, photo, index, rect)| {
                let view = s.face_view(PhotoId(photo), rect);
                json!({"photo": photo, "index": index, "rect": super::rect_json(&rect), "view": super::rect_json(&view), "score": score})
            })
            .collect();
        Similar { items, ..ready }
    }

    /// Most faces put in order of likeness (the order is a pass over every pair of them).
    const MAX_ARRANGED: usize = 1500;

    /// Faces in an order that puts look-alikes together: start anywhere, then always go to the closest face not yet placed.
    fn nearest_chain(embeddings: &[&[f32]]) -> Vec<usize> {
        let n = embeddings.len();
        let (mut placed, mut order) = (vec![false; n], Vec::with_capacity(n));
        let mut current = 0;
        if n == 0 {
            return order;
        }
        placed[0] = true;
        order.push(0);
        for _ in 1..n {
            let mut best: Option<(usize, f32)> = None;
            for (j, e) in embeddings.iter().enumerate() {
                if placed[j] {
                    continue;
                }
                let c = matching::cosine(embeddings[current], e).unwrap_or(-1.0);
                if best.is_none_or(|(_, b)| c > b) {
                    best = Some((j, c));
                }
            }
            let Some((j, _)) = best else { break };
            placed[j] = true;
            order.push(j);
            current = j;
        }
        order
    }

    /// Put the unnamed faces `all` in order of likeness (the ones already embedded; the rest follow in photo order) and
    /// find a name for those the named faces recognise, for the first `limit` of them.
    pub fn arrange_unnamed(s: &mut Session, all: &mut Vec<(u64, usize, Rect)>, limit: usize) -> super::Arranged {
        let none = super::Arranged { ordered: false, ready: false, suggestions: Default::default(), groups: Default::default() };
        let Some(_) = s.face_models_dir.clone() else { return none };
        let Ok(manifest) = current_manifest(s, "faces.unnamed", false) else { return none };
        let (mut embedded, mut plain) = (Vec::new(), Vec::new());
        for f in all.drain(..) {
            match s.faces.index.get(f.0, &f.2) {
                Some(e) => embedded.push((f, e.clone())),
                None => plain.push(f),
            }
        }
        let beyond = embedded.split_off(embedded.len().min(MAX_ARRANGED));
        let order = nearest_chain(&embedded.iter().map(|(_, e)| &e[..]).collect::<Vec<_>>());
        let ordered: Vec<((u64, usize, Rect), Arc<[f32]>)> = order.into_iter().filter_map(|i| embedded.get(i).cloned()).collect();
        // names the faces already named suggest, as `faces.suggest` would
        let gallery: Vec<(String, Arc<[f32]>)> = s
            .catalog
            .photos()
            .filter(|ph| ph.in_library())
            .flat_map(|ph| {
                ph.meta
                    .regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Face)
                    .filter_map(|r| Some((r.name.clone()?, s.faces.index.get(ph.id.0, &r.rect)?.clone())))
                    .collect::<Vec<_>>()
            })
            .collect();
        let refs: Vec<(&str, &[f32])> = gallery.iter().map(|(n, e)| (n.as_str(), &e[..])).collect();
        let threshold = manifest.thresholds.match_cosine.unwrap_or(DEFAULT_THRESHOLD);
        let top = top_faces(s);
        let mut suggestions = std::collections::HashMap::new();
        if !refs.is_empty() {
            for ((photo, index, _), e) in ordered.iter().take(limit) {
                if let Some(x) = matching::suggest(&matching::rank_top(e, &refs, top), threshold, DEFAULT_MARGIN) {
                    suggestions.insert((*photo, *index), (x.name, x.score));
                }
            }
        }
        let groups = look_alike_groups(ordered.iter().map(|((photo, index, _), e)| ((*photo, *index), &e[..])), threshold);
        let grouped = !ordered.is_empty();
        all.extend(ordered.into_iter().map(|(f, _)| f));
        all.extend(beyond.into_iter().map(|(f, _)| f));
        all.extend(plain);
        super::Arranged { ordered: grouped, ready: true, suggestions, groups }
    }

    /// Number the runs of `chain` (faces in order of likeness) whose neighbours are at least `threshold` alike: each
    /// run is one look-alike group. A pair that cannot be compared starts a new group.
    pub(super) fn look_alike_groups<'a>(
        chain: impl Iterator<Item = ((u64, usize), &'a [f32])>,
        threshold: f32,
    ) -> std::collections::HashMap<(u64, usize), usize> {
        let mut groups = std::collections::HashMap::new();
        let mut group = 0usize;
        let mut prev: Option<&[f32]> = None;
        for (key, e) in chain {
            if let Some(p) = prev
                && !matching::cosine(p, e).is_some_and(|c| c >= threshold)
            {
                group = group.saturating_add(1);
            }
            groups.insert(key, group);
            prev = Some(e);
        }
        groups
    }

    /// How many of a person's best matching faces a match is based on (Settings ▸ Faces), looked up at most once a second.
    fn top_faces(s: &mut Session) -> usize {
        if let Some((t, n)) = s.faces.top_seen
            && t.elapsed() < RECHECK
        {
            return n;
        }
        let n = s.face_models_dir.as_deref().map_or(matching::TOP_FACES, |d| read_settings(d).top_faces());
        s.faces.top_seen = Some((Instant::now(), n));
        n
    }

    /// `topFaces` asked for in a command (1 to 8, refused otherwise, as `faces.matching` refuses it), else the setting.
    fn top_faces_param(s: &mut Session, cmd: &str, p: &Value) -> Result<usize> {
        let asked = match p.get("topFaces") {
            Some(v) => top_faces_value(cmd, v, "the count chosen in Settings")?,
            None => None,
        };
        Ok(asked.unwrap_or_else(|| top_faces(s)))
    }

    pub fn suggest(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.suggest";
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(1500).min(600_000));
        let manifest = current_manifest(s, C, !budget.is_zero())?;
        collect(s);
        let targets = s.targets(p)?;
        let model_threshold = manifest.thresholds.match_cosine;
        let num = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as f32);
        let threshold = num("threshold").or(model_threshold).unwrap_or(DEFAULT_THRESHOLD);
        let margin = num("margin").unwrap_or(DEFAULT_MARGIN);
        let top = top_faces_param(s, C, p)?;
        // the photos asked about are embedded first, then the named faces they are compared with, within the budget
        let started = Instant::now();
        let todo = pending(s, &targets, false);
        let mut done = 0;
        if !budget.is_zero() && !todo.is_empty() {
            let embedder = current(s, C)?;
            for id in &todo {
                if done > 0 && started.elapsed() >= budget {
                    break;
                }
                embed_photo(s, *id, &embedder)?;
                done += 1;
            }
        }
        let pending_photos: Vec<u64> = todo.iter().skip(done).map(|id| id.0).collect();
        let gallery: Vec<(String, Arc<[f32]>)> = s
            .catalog
            .photos()
            .filter(|ph| ph.in_library())
            .flat_map(|ph| {
                ph.meta
                    .regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Face)
                    .filter_map(|r| Some((r.name.clone()?, s.faces.index.get(ph.id.0, &r.rect)?.clone())))
                    .collect::<Vec<_>>()
            })
            .collect();
        let refs: Vec<(&str, &[f32])> = gallery.iter().map(|(n, e)| (n.as_str(), &e[..])).collect();
        let mut photos = Vec::new();
        for id in &targets {
            let Some(photo) = s.catalog.photo(*id) else { continue };
            let mut faces = Vec::new();
            for (index, r) in photo.meta.regions.iter().enumerate().filter(|(_, r)| r.kind == RegionKind::Face && r.name.is_none()) {
                let Some(q) = s.faces.index.get(id.0, &r.rect) else {
                    faces.push(json!({"index": index, "pending": true}));
                    continue;
                };
                let ranked = matching::rank_top(q, &refs, top);
                let suggestion = matching::suggest(&ranked, threshold, margin);
                faces.push(json!({
                    "index": index,
                    "suggestion": suggestion.as_ref().map(|x| json!({"name": x.name, "score": x.score, "runnerUp": x.runner_up.as_ref().map(|(n, sc)| json!({"name": n, "score": sc}))})),
                    "candidates": ranked.iter().take(3).map(|c| json!({"name": c.name, "score": c.score, "faces": c.faces})).collect::<Vec<_>>(),
                }));
            }
            photos.push(json!({"id": id.0, "faces": faces}));
        }
        Ok(
            json!({"model": s.faces.index.tag, "threshold": threshold, "margin": margin, "topFaces": top, "galleryFaces": gallery.len(), "pendingPhotos": pending_photos, "photos": photos}),
        )
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::tests_face_recognize::{install_face_model, region, set_regions, setup, temp, two_photos};

        /// #259: runs of look-alikes share a group; an unlike neighbour, or one that cannot be compared, starts a new one.
        #[test]
        fn look_alikes_next_to_each_other_form_groups() {
            let (a, a2, b, bad) = (unit(&[(0, 1.0)]), unit(&[(0, 1.0), (1, 0.1)]), unit(&[(5, 1.0)]), vec![f32::NAN; 64]);
            let chain = [((1, 0), &a), ((2, 0), &a2), ((3, 0), &b), ((4, 0), &bad), ((5, 0), &b)];
            let g = look_alike_groups(chain.iter().map(|(k, e)| (*k, &e[..])), 0.5);
            assert_eq!(g[&(1, 0)], g[&(2, 0)]);
            assert_ne!(g[&(2, 0)], g[&(3, 0)]);
            assert_ne!(g[&(3, 0)], g[&(4, 0)]);
            assert_ne!(g[&(4, 0)], g[&(5, 0)]);
            assert!(look_alike_groups(std::iter::empty(), 0.5).is_empty());
        }

        fn unit(parts: &[(usize, f32)]) -> Vec<f32> {
            let mut v = vec![0.0; 64];
            for (i, x) in parts {
                v[*i] = *x;
            }
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter().map(|x| x / n).collect()
        }

        /// A session with a tiny model loaded (so the index has its model's tag).
        fn loaded(name: &str) -> (Session, std::path::PathBuf) {
            let d = temp(name);
            let (mut s, _) = setup(&d, 64);
            s.execute("faces.index", &json!({"budgetMs": 0})).unwrap();
            (s, d)
        }

        fn finished(s: &Session, id: PhotoId, found: Option<Vec<Rect>>, results: Vec<(Rect, Option<Vec<f32>>, Rect)>) -> Done {
            let content = s.catalog.photo(id).map(|p| crate::media::content_key(p)).unwrap_or_default();
            Done { tag: s.faces.index.tag.clone(), epoch: s.faces.epoch, id, content, results, detect_wanted: true, found }
        }

        #[test]
        fn the_background_scan_finds_faces_once_and_only_in_photos_without_any() {
            // faces found by the scan become unnamed regions without an undo step
            {
                let (mut s, d) = loaded("found");
                let (a, b) = two_photos(&s);
                set_regions(&mut s, b, vec![region(0.1, Some("Ann"))]);
                let (undo, redo) = (s.undo.len(), s.redo.len());
                let found = region(0.3, None).rect;
                let done = finished(&s, a, Some(vec![found]), vec![(found, Some(unit(&[(0, 1.0)])), found)]);
                assert_eq!(ingest(&mut s, done), (1, 0));
                let regions = &s.catalog.photo(a).unwrap().meta.regions;
                assert_eq!(regions.len(), 1);
                assert!(regions[0].kind == RegionKind::Face && regions[0].name.is_none() && regions[0].rect == found);
                assert!(regions[0].description.as_deref().unwrap().starts_with(MARK), "marked as detected");
                // not something the user did, and its embedding is already there
                assert_eq!((s.undo.len(), s.redo.len()), (undo, redo));
                assert!(s.skip_auto_write, "no sidecar is rewritten for it");
                assert!(s.faces.scanned.contains(a.0) && s.faces.index.contains(a.0, &found));
                // a photo that has faces by now (the user, an import) keeps exactly those
                let other = region(0.6, None).rect;
                let done = finished(&s, b, Some(vec![other]), vec![(other, None, other)]);
                ingest(&mut s, done);
                assert_eq!(s.catalog.photo(b).unwrap().meta.regions.len(), 1);
                assert!(s.faces.scanned.contains(b.0));
                let _ = std::fs::remove_dir_all(&d);
            }
            // a photo that could not be searched is left alone for the session
            {
                let (mut s, d) = loaded("scanfail");
                let (a, _) = two_photos(&s);
                assert!(needs_search(&s, s.catalog.photo(a).unwrap()));
                let done = finished(&s, a, None, vec![]);
                ingest(&mut s, done);
                assert!(!s.faces.scanned.contains(a.0), "not remembered as searched: the file may come back");
                assert!(!needs_search(&s, s.catalog.photo(a).unwrap()), "but not retried on every pass");
                assert!(!pending(&s, &[], true).contains(&a));
                let _ = std::fs::remove_dir_all(&d);
            }
            // only the background scan looks for faces and only in photos without any
            {
                let (mut s, d) = loaded("search");
                // finding faces needs the detector, which is not bundled; without it the scan only embeds
                let detector = install_face_model(&mut s);
                let (a, b) = two_photos(&s);
                set_regions(&mut s, b, vec![region(0.1, Some("Ann")), region(0.5, None)]);
                // `faces.index` embeds what is boxed; only the scan (search = true) also searches the rest, last
                assert_eq!(pending(&s, &[], false), vec![b]);
                let scan = pending(&s, &[], true);
                assert_eq!(scan.first(), Some(&b), "named faces first");
                assert!(scan.contains(&a) && scan.len() > 2, "the unsearched photos follow");
                assert!(prepare(&mut s, a, false).is_none(), "nothing to embed in a photo without boxes");
                if detector {
                    assert!(prepare(&mut s, a, true).is_some_and(|p| p.detect && p.todo.is_empty()));
                } else {
                    assert!(prepare(&mut s, a, true).is_none(), "no detector: nothing to do, and not marked as searched");
                }
                assert!(prepare(&mut s, b, true).is_some_and(|p| !p.detect && p.todo.len() == 2), "a photo with boxes is not searched");
                // once searched, a photo is not searched again
                s.faces.scanned.insert(a.0);
                assert!(!pending(&s, &[], true).contains(&a));
                let _ = std::fs::remove_dir_all(&d);
            }
        }

        #[test]
        fn faces_from_another_library_or_a_replaced_file_are_dropped() {
            // opening another library forgets the old ones faces
            {
                let (mut s, d) = loaded("newlib");
                let (a, _) = two_photos(&s);
                let rect = region(0.2, None).rect;
                s.faces.index.insert(a.0, &rect, unit(&[(0, 1.0)]));
                s.faces.scanned.insert(a.0);
                let epoch = s.faces.epoch;
                let old = finished(&s, a, Some(vec![rect]), vec![(rect, Some(unit(&[(1, 1.0)])), rect)]);
                s.open_library(d.join("second"), true).unwrap();
                assert_eq!((s.faces.index.len(), s.faces.scanned.len(), s.faces.queue.len(), s.faces.in_flight.len()), (0, 0, 0, 0));
                assert!(s.faces.epoch != epoch && s.faces.embedder.is_none());
                // a worker that was still busy with the old library cannot put its faces into the new one
                assert_eq!(ingest(&mut s, old), (0, 0));
                assert!(s.faces.index.len() == 0 && !s.faces.scanned.contains(a.0));
                let _ = std::fs::remove_dir_all(&d);
            }
            // faces made from a replaced file are dropped
            // A Reload that finds the file replaced (an outside edit) gives the photo other pixels: what a worker made from
            // the old file is dropped when it comes back, the faces already embedded from it are forgotten and the photo is
            // searched again (issue #1524).
            {
                let (mut s, d) = loaded("replaced");
                let (a, b) = two_photos(&s);
                let rect = region(0.2, None).rect;
                s.faces.index.insert(a.0, &rect, unit(&[(0, 1.0)]));
                s.faces.scanned.insert(a.0);
                s.faces.index.insert(b.0, &rect, unit(&[(1, 1.0)]));
                s.faces.scanned.insert(b.0);
                let old = finished(&s, a, Some(vec![rect]), vec![(rect, Some(unit(&[(2, 1.0)])), rect)]);
                let (width, height) = s.catalog.photo(a).map(|p| (p.width, p.height)).unwrap();
                let replace = Op::SetContent {
                    id: a,
                    width,
                    height,
                    file_size: 1234,
                    content_hash: Some("edited".into()),
                    preview_only: None,
                    preview_checked: String::new(),
                };
                s.commit("Reload Changed Files", replace).unwrap();
                assert!(s.faces.index.get(a.0, &rect).is_none() && !s.faces.scanned.contains(a.0), "the old file's faces are forgotten");
                assert!(needs_search(&s, s.catalog.photo(a).unwrap()), "and the photo is searched again");
                assert!(s.faces.index.get(b.0, &rect).is_some() && s.faces.scanned.contains(b.0), "other photos keep theirs");
                assert_eq!(ingest(&mut s, old), (0, 0), "a result made from the old file is dropped");
                assert!(s.catalog.photo(a).unwrap().meta.regions.is_empty() && !s.faces.scanned.contains(a.0));
                // what is made from the new file is taken in
                let new = finished(&s, a, Some(vec![rect]), vec![(rect, Some(unit(&[(3, 1.0)])), rect)]);
                assert_eq!(ingest(&mut s, new), (1, 0));
                assert!(s.faces.scanned.contains(a.0) && s.faces.index.get(a.0, &rect).is_some());
                let _ = std::fs::remove_dir_all(&d);
            }
        }

        #[test]
        fn the_unnamed_faces_that_look_like_a_person_come_best_first() {
            let (mut s, d) = loaded("similar");
            let (a, b) = two_photos(&s);
            let c = s.catalog.photos().map(|p| p.id).find(|id| *id != a && *id != b).unwrap();
            set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, Some("Bob"))]);
            set_regions(&mut s, b, vec![region(0.1, None), region(0.5, None)]);
            set_regions(&mut s, c, vec![region(0.3, None)]);
            let put = |s: &mut Session, id: PhotoId, x: f64, v: Vec<f32>| s.faces.index.insert(id.0, &region(x, None).rect, v);
            put(&mut s, a, 0.1, unit(&[(0, 1.0)])); // Ann
            put(&mut s, a, 0.5, unit(&[(1, 1.0)])); // Bob
            put(&mut s, b, 0.1, unit(&[(0, 0.95), (1, 0.05)])); // looks just like Ann
            put(&mut s, b, 0.5, unit(&[(1, 1.0)])); // looks like Bob, not Ann
            put(&mut s, c, 0.3, unit(&[(0, 0.8), (1, 0.6)])); // somewhat like Ann
            let r = similar(&mut s, "ann", 10, &None);
            assert!(r.ready);
            let found: Vec<(u64, u64)> = r.items.iter().map(|i| (i["photo"].as_u64().unwrap(), i["index"].as_u64().unwrap())).collect();
            assert_eq!(found, vec![(b.0, 0), (c.0, 0)], "best first; the face that looks like Bob is left out");
            assert!(r.items[0]["score"].as_f64().unwrap() > r.items[1]["score"].as_f64().unwrap());
            assert_eq!(similar(&mut s, "ann", 1, &None).items.len(), 1, "capped");
            assert!(similar(&mut s, "nobody", 10, &None).items.is_empty());
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use serde_json::Value;

    use super::super::bad;
    use crate::{Result, Session};

    pub fn index(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.index", "this build cannot run face recognition"))
    }

    pub fn suggest(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.suggest", "this build cannot run face recognition"))
    }

    pub fn evaluate(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.evaluate", "this build cannot run face recognition"))
    }

    pub fn pump(_: &mut Session, _: &Value) -> Result<Value> {
        Ok(serde_json::json!({"active": false}))
    }

    pub fn status(_: &mut Session, _: &Value) -> Result<Value> {
        Ok(
            serde_json::json!({"enabled": false, "paused": false, "state": "off", "done": 0, "total": 0, "pendingPhotos": 0, "indexedFaces": 0, "searchedPhotos": 0}),
        )
    }

    pub fn clear(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.clearIndex", "this build cannot run face recognition"))
    }

    pub fn restart(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.restart", "this build cannot run face recognition"))
    }

    pub fn similar(_: &mut Session, _: &str, _: usize, _: &super::Scope) -> super::Similar {
        super::Similar { items: Vec::new(), ready: false, pending: 0 }
    }

    pub fn arrange_unnamed(_: &mut Session, _: &mut Vec<(u64, usize, lcc_geom::Rect)>, _: usize) -> super::Arranged {
        super::Arranged { ordered: false, ready: false, suggestions: Default::default(), groups: Default::default() }
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "faces.ignore",
            "Ignore Faces",
            [],
            None,
            "{faces: [{photo, index, rect?}], ignored?: true}: ignore blurry or false faces (kept, never detected again, left out of People), or restore them with ignored: false; one undo step; catalog only",
            super::always,
            ignore_faces
        ),
        cmd!(query "faces.ignored", "Ignored Faces", [], None,
            "{offset?: 0, limit?: 1000} → {total, offset, faces: [{photo, index, rect, name, view}]}: the ignored faces in catalog order; limit 1..5000", super::always, ignored_faces),
        cmd!(
            "faces.index",
            "Index Faces",
            [],
            None,
            "{budgetMs?: 200, ids?: [photo ids to do first]} → {embedded, photosDone, pendingPhotos, indexedFaces, ms}: embed the faces in the library with the chosen recognition model, as many photos as fit in the time (at least one); call again until `pendingPhotos` is 0. `budgetMs: 0` only reports",
            super::always,
            imp::index
        ),
        cmd!(
            "faces.suggest",
            "Suggest Names for Faces",
            [],
            None,
            "{ids?, threshold?, margin?, topFaces?: 1..8 | null, budgetMs?} → {topFaces, photos: [{id, faces: [{index, suggestion: {name, score, runnerUp} | null, candidates: [{name, score}]}]}]}: who the unnamed faces in these photos might be, from the faces already named in the library. Only suggestions: nothing is named",
            super::always,
            imp::suggest
        ),
        cmd!(
            query "faces.pump",
            "Index Faces in the Background",
            [],
            None,
            "{pace?: pause | light | normal | full} → {active, inFlight, pendingPhotos, indexedFaces, searchedPhotos, workers}: cheap to call every frame: takes in what the background workers have finished and gives them the next photos, as many at once as the pace allows (none when paused, one when light, half the machine's threads when normal, four fifths when full); inactive until face recognition is on in Settings with a model chosen",
            super::always,
            imp::pump
        ),
        cmd!(
            query "faces.status",
            "Face Scan Status",
            [],
            None,
            "{} → {enabled, paused, state: off | idle | indexing | paused | complete, done, total, pendingPhotos, indexedFaces, searchedPhotos}: how far the background face scan is (`done` of `total`: photos since it last had nothing to do)",
            super::always,
            imp::status
        ),
        cmd!(
            "faces.pause",
            "Pause Face Scan",
            [],
            None,
            "{} → {enabled, paused}: pause the background face scan (what is running finishes, nothing new starts); it stays paused across restarts until `faces.resume`",
            super::always,
            |s, _| super::face_models::set_paused(s, "faces.pause", true)
        ),
        cmd!(
            "faces.resume",
            "Resume Face Scan",
            [],
            None,
            "{} → {enabled, paused}: resume a paused background face scan",
            super::always,
            |s, _| super::face_models::set_paused(s, "faces.resume", false)
        ),
        cmd!(
            "faces.clearIndex",
            "Clear Face Index",
            [],
            None,
            "{} → as faces.status: drop what the face scan learned for this library (every face embedding, and which photos were searched) and pause the scan. Faces already in the catalog, named or found, stay. Destructive: frontends ask first",
            super::always,
            imp::clear
        ),
        cmd!(
            "faces.restart",
            "Restart Face Scan",
            [],
            None,
            "{} → as faces.status: drop the face index (as faces.clearIndex) and scan the library again from the start (resumes a paused scan)",
            super::always,
            imp::restart
        ),
        cmd!(
            "faces.evaluate",
            "Evaluate Face Recognition",
            [],
            None,
            "{budgetMs?: 0, burstSecs?: 5, margin?, thresholds?: [..], topFaces?: 1..8 | null, scope?: \"visible\"} → {queries, top1, topFaces, sweep: [{threshold, suggested, correct, precision, recall}]}: leave-one-out over your named faces: each is ranked against the others (ignoring shots within burstSecs of it) to show how well the chosen model recognises people in *your* photos and which threshold to trust. Faces not yet embedded are done within budgetMs (`faces.index` does them all); counts only",
            super::always,
            imp::evaluate
        ),
        cmd!(
            query "faces.unnamed",
            "Unnamed Faces",
            [],
            None,
            "{limit?: 600, scope?: \"filter\"} → {total, faces: [{photo, index, rect, view, suggestion: {name, score} | null}], ordered, ready}: the faces nobody has named (with `scope: \"filter\"`, only in the photos the library filter shows, any person filter aside); with a recognition model chosen, look-alikes are next to each other and a face the named ones recognise carries the name they suggest. `view` is the box to show the face by. Name them with `faces.nameFaces`",
            super::always,
            unnamed
        ),
        cmd!(
            "faces.nameFaces",
            "Name Faces",
            [],
            None,
            "{faces: [{photo, index}], name} → {named, photos}: name (or, with a null or empty name, clear) many face regions as one undoable step; the sidecar is not touched",
            super::always,
            name_faces
        ),
        cmd!(
            query "faces.person",
            "Faces of a Person",
            [],
            None,
            "{name, more?: 120, scope?: \"filter\"} → {name, total, confirmed: [{photo, index, rect}], more: [{photo, index, rect, score}], ready, pendingPhotos}: the faces named `name` across the library, and the unnamed faces that look most like them (best first, already embedded ones only, once a recognition model is chosen); confirm one with `faces.setName`. `scope: \"filter\"` shows only the faces in the photos the library filter shows (any person filter aside); `total` still counts the whole library",
            super::always,
            person
        ),
        cmd!(
            "faces.setName",
            "Name Face",
            [],
            None,
            "{id?, index, name}: name a face region (null or empty clears it); undoable; the sidecar is not touched",
            super::always,
            set_name
        ),
        cmd!(
            "faces.renamePerson",
            "Rename Person",
            [],
            None,
            "{from, to, merge?: bool} → {faces, photos, from, to, merged}: give every in-library face of `from` the name `to` (trimmed, any capitalization), one undo step; refused when `to` is already someone else unless `merge` is true, which combines the two; the sidecar is not touched",
            super::always,
            rename_person
        ),
    ]
}
