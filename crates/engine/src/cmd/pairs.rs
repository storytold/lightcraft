//! RAW+JPEG pair commands. Detection and links live in the catalog; these commands join, split
//! and copy culling metadata. Presentation is `library.pairPreferences` plus `photo.pairFocus`.

use lightcraft_catalog::{MediaKind, Op, PairId, PhotoId, RawJpegPair, Source};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_selection, str_param};
use crate::{Result, Session};

fn file_path(p: &lightcraft_catalog::Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } => Some(path),
        Source::Demo { .. } => None,
    }
}

fn is_jpeg_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

fn candidates(s: &Session) -> Value {
    let scan = s.catalog.scan_pairs();
    let id = |p: PhotoId| p.0;
    json!({
        "pairs": scan.pairs.iter().map(|p| json!({"raw": id(p.raw), "jpeg": id(p.jpeg)})).collect::<Vec<_>>(),
        "ambiguous": scan.ambiguous.iter().map(|a| json!({
            "directory": a.directory,
            "stem": a.stem,
            "reason": a.reason,
            "ids": a.ids.iter().map(|p| id(*p)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "exports": scan.exports.iter().map(|e| json!({"raw": id(e.raw), "jpeg": id(e.jpeg)})).collect::<Vec<_>>(),
    })
}

/// Detected pairs, plus a stem the user split when `ids` names one of its photos.
fn chosen(s: &Session, ids: Option<&[PhotoId]>) -> Vec<(PhotoId, PhotoId, String, String)> {
    let scan = s.catalog.scan_pairs();
    let mut out = Vec::new();
    for cand in &scan.pairs {
        if s.catalog.pair_of(cand.raw).is_some() {
            continue;
        }
        if ids.is_some_and(|ids| !ids.contains(&cand.raw) && !ids.contains(&cand.jpeg)) {
            continue;
        }
        if let Some(key) = rejection_key(s, cand.raw) {
            out.push((cand.raw, cand.jpeg, key.0, key.1));
        }
    }
    if let Some(ids) = ids {
        for amb in scan.ambiguous.iter().filter(|a| a.reason == "rejected") {
            let raw = amb.ids.iter().copied().find(|id| s.catalog.photo(*id).is_some_and(|p| p.kind == MediaKind::Raw));
            let jpeg = amb.ids.iter().copied().find(|id| s.catalog.photo(*id).is_some_and(|p| is_jpeg_name(&p.file_name)));
            let (Some(raw), Some(jpeg)) = (raw, jpeg) else { continue };
            if !ids.contains(&raw) && !ids.contains(&jpeg) {
                continue;
            }
            if s.catalog.pair_of(raw).is_some() {
                continue;
            }
            let (Some(raw_p), Some(jpeg_p)) = (s.catalog.photo(raw), s.catalog.photo(jpeg)) else { continue };
            if !lightcraft_catalog::pairs::photos_match(raw_p, jpeg_p) {
                continue;
            }
            out.push((raw, jpeg, amb.directory.clone(), amb.stem.clone()));
        }
    }
    out
}

fn rejection_key(s: &Session, raw: PhotoId) -> Option<(String, String)> {
    let photo = s.catalog.photo(raw)?;
    let path = file_path(photo)?;
    lightcraft_catalog::pairs::location_key(path, &photo.file_name)
}

fn join(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = p.get("ids").map(|_| s.targets(p)).transpose()?;
    let chosen = chosen(s, ids.as_deref());
    let mut ops = Vec::new();
    for (raw, jpeg, dir, stem) in &chosen {
        if s.catalog.pair_rejected(dir, stem) {
            ops.push(Op::RejectPair { directory: dir.clone(), stem: stem.clone(), rejected: false });
        }
        let id = s.catalog.alloc_pair_id();
        ops.push(Op::AddPair { pair: RawJpegPair { id, raw: *raw, jpeg: *jpeg } });
    }
    let n = chosen.len();
    if !ops.is_empty() {
        s.end_interaction()?;
        s.commit("Join RAW+JPEG", Op::Batch { ops })?;
        if s.paired_culling {
            show_paired_jpegs(s);
        }
    }
    Ok(json!({"joined": n}))
}

fn split(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = s.targets(p)?;
    let mut ops = Vec::new();
    let mut seen: Vec<PairId> = Vec::new();
    for id in ids {
        let Some(pair) = s.catalog.pair_of(id).cloned() else { continue };
        if seen.contains(&pair.id) {
            continue;
        }
        seen.push(pair.id);
        ops.push(Op::RemovePair { id: pair.id });
        if let Some((dir, stem)) = rejection_key(s, pair.raw) {
            ops.push(Op::RejectPair { directory: dir, stem, rejected: true });
        }
        s.pair_show_raw.remove(&pair.id);
    }
    let n = seen.len();
    if !ops.is_empty() {
        s.commit("Split RAW+JPEG", Op::Batch { ops })?;
    }
    Ok(json!({"split": n}))
}

fn sync_meta(s: &mut Session, p: &Value) -> Result<Value> {
    let from_jpeg = match str_param(p, "from") {
        Some("raw") => false,
        Some("jpeg") => true,
        _ => return Err(bad("photo.syncPairMetadata", "from must be raw or jpeg")),
    };
    let mut ops = Vec::new();
    let mut seen: Vec<PairId> = Vec::new();
    for id in s.targets(p)? {
        let Some(pair) = s.catalog.pair_of(id).cloned() else { continue };
        if !seen.contains(&pair.id) {
            seen.push(pair.id);
        } else {
            continue;
        }
        let (src, dst) = if from_jpeg { (pair.jpeg, pair.raw) } else { (pair.raw, pair.jpeg) };
        let Some(source) = s.catalog.photo(src).cloned() else { continue };
        let Some(dest) = s.catalog.photo(dst) else { continue };
        if dest.rating != source.rating {
            ops.push(Op::SetRating { id: dst, rating: source.rating });
        }
        if dest.flag != source.flag {
            ops.push(Op::SetFlag { id: dst, flag: source.flag });
        }
        if dest.label != source.label {
            ops.push(Op::SetLabel { id: dst, label: source.label });
        }
    }
    let n = ops.len();
    if n > 0 {
        s.commit("Sync RAW+JPEG Metadata", Op::Batch { ops })?;
    }
    Ok(json!({"changed": n}))
}

fn prefs(s: &mut Session, p: &Value) -> Result<Value> {
    let mut changed = false;
    if let Some(v) = p.get("pairedCulling").and_then(Value::as_bool) {
        if v && !s.paired_culling {
            s.end_interaction()?;
            s.paired_culling = true;
            s.pair_show_raw.clear();
            show_paired_jpegs(s);
        }
        s.paired_culling = v;
        changed = true;
    }
    if let Some(v) = p.get("syncPairMetadata").and_then(Value::as_bool) {
        s.sync_pair_metadata = v;
        changed = true;
    }
    if let Some(v) = p.get("syncPairLabels").and_then(Value::as_bool) {
        s.sync_pair_labels = v;
        changed = true;
    }
    if changed {
        s.save_prefs()?;
    }
    Ok(json!({
        "pairedCulling": s.paired_culling,
        "syncPairMetadata": s.sync_pair_metadata,
        "syncPairLabels": s.sync_pair_labels,
    }))
}

/// Move a hidden selected raw to its visible JPEG, preserving focused raws and stacks.
fn show_paired_jpegs(s: &mut Session) {
    s.pair_focus_gen = s.pair_focus_gen.saturating_add(1);
    let visible: std::collections::HashSet<_> = s.visible_cloned().into_iter().collect();
    let shown = |id| {
        s.catalog
            .pair_of(id)
            .filter(|pair| pair.raw == id && !visible.contains(&id) && visible.contains(&pair.jpeg))
            .map(|pair| pair.jpeg)
            .unwrap_or(id)
    };
    let mut ids = Vec::new();
    for id in s.selection.ids.clone() {
        let id = shown(id);
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let active = s.selection.active.map(shown);
    s.selection.ids = ids;
    s.selection.active = active.or_else(|| s.selection.ids.last().copied());
    s.active_mask = None;
    s.active_spot = None;
}

fn focus(s: &mut Session, p: &Value) -> Result<Value> {
    let which_jpeg = match str_param(p, "which") {
        Some("raw") => false,
        Some("jpeg") => true,
        _ => return Err(bad("photo.pairFocus", "which must be raw or jpeg")),
    };
    let pairs: Vec<_> = s.targets(p)?.into_iter().filter_map(|id| s.catalog.pair_of(id).cloned()).collect();
    let previous = s.pair_show_raw.clone();
    for pair in &pairs {
        if which_jpeg {
            s.pair_show_raw.remove(&pair.id);
        } else {
            s.pair_show_raw.insert(pair.id);
        }
    }
    let changed = pairs.len();
    if let Some(pair) = pairs.last() {
        s.pair_focus_gen = s.pair_focus_gen.saturating_add(1);
        let visible = s.visible_cloned();
        if pairs.iter().any(|pair| !visible.contains(&if which_jpeg { pair.jpeg } else { pair.raw })) {
            s.pair_show_raw = previous;
            s.pair_focus_gen = s.pair_focus_gen.saturating_add(1);
            return Err(bad("photo.pairFocus", "the chosen file is excluded by the current view; clear the filter or show both files first"));
        }
        if let Err(error) = s.end_interaction() {
            s.pair_show_raw = previous;
            s.pair_focus_gen = s.pair_focus_gen.saturating_add(1);
            return Err(error);
        }
        s.selection = crate::Selection::single(if which_jpeg { pair.jpeg } else { pair.raw });
        s.active_mask = None;
        s.active_spot = None;
    }
    Ok(json!({"focused": if which_jpeg { "jpeg" } else { "raw" }, "changed": changed }))
}

fn explicit(mut spec: CommandSpec) -> CommandSpec {
    spec.explicit_targets = true;
    spec
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "photo.pairCandidates",
            "RAW+JPEG Candidates",
            [],
            None,
            "{} → {pairs: [{raw, jpeg}], ambiguous: [{directory, stem, reason, ids}], exports: [{raw, jpeg}]}",
            always,
            |s, _| Ok(candidates(s))
        ),
        cmd!(
            "photo.joinPairs",
            "Join RAW+JPEG",
            ["Photo"],
            None,
            "{ids?} — link detected pairs (ids: only pairs touching those photos, including a stem that was split). Differing ratings stay until the next cull or photo.syncPairMetadata → {joined}",
            always,
            join
        ),
        explicit(cmd!(
            "photo.splitPair",
            "Split RAW+JPEG",
            ["Photo"],
            None,
            "{ids?} — drop the link and remember the stem so a scan does not join it again. Both photos stay → {split}",
            has_selection,
            split
        )),
        explicit(cmd!(
            "photo.syncPairMetadata",
            "Sync RAW+JPEG Metadata",
            ["Photo"],
            None,
            "{ids?, from: raw|jpeg} — copy rating, flag and colour label one way → {changed}",
            has_selection,
            sync_meta
        )),
        cmd!(
            "library.pairPreferences",
            "RAW+JPEG Pairing",
            ["File"],
            None,
            "{pairedCulling?: bool, syncPairMetadata?: bool, syncPairLabels?: bool} → current preferences (saved with the library). Off by default",
            always,
            prefs
        ),
        explicit(cmd!(
            "photo.pairFocus",
            "Edit RAW or JPEG",
            ["Photo"],
            None,
            "{which: raw|jpeg, ids?} — which file of a pair the grid shows and Develop edits. Does not copy develop settings",
            has_selection,
            focus
        )),
    ]
}
