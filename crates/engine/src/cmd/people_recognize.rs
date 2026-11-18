//! `people.recognize` and `people.cluster`: telling who is on photos by their look (clothes, hair,
//! SAM 2.1's features of the body; a face embedding when one is installed), against the people
//! named on other photos (`people.learn`), and grouping the unnamed faces of a shoot. Appearance
//! only holds within a shoot, so the examples of the photo's own folder are used first.
//!
//! Ported from OneSlips' fork (own design there), on the app's own appearance cache
//! ([`Session::photo_appearance`]).
#![cfg_attr(not(feature = "sam"), allow(dead_code, unused_imports))]

use lcc_catalog::{Op, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, bad, bool_or, cmd};
use crate::identity::FoundFace;
use crate::{Result, Session};

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

/// Suggestions need this score (the appearance model's probability of the same person).
pub const SUGGEST: f32 = 0.6;

fn iou(a: [f64; 4], b: [f64; 4]) -> f64 {
    let ix = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let iy = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = ix * iy;
    let union = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0) + (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0) - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// The photo's Face region that is this face (overlapping it, or around its centre).
fn region_of(meta: &lcc_catalog::Meta, rect: [f64; 4]) -> Option<usize> {
    let c = [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0];
    meta.regions
        .iter()
        .enumerate()
        .filter(|(_, r)| r.kind == lcc_meta::RegionKind::Face)
        .map(|(i, r)| {
            let b = [r.rect.x0, r.rect.y0, r.rect.x1, r.rect.y1];
            let inside = (b[0]..=b[2]).contains(&c[0]) && (b[1]..=b[3]).contains(&c[1]);
            (i, iou(b, rect).max(if inside { 0.3 } else { 0.0 }))
        })
        .filter(|(_, s)| *s >= 0.3)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

fn is_named(meta: &lcc_catalog::Meta, f: &FoundFace) -> bool {
    region_of(meta, f.rect).and_then(|r| meta.regions.get(r)).is_some_and(|r| r.name.as_deref().is_some_and(|n| !n.trim().is_empty()))
}

/// Faces on the photos `ids` (computing or queueing them): `(ready, pending photos)`.
fn faces_of(s: &mut Session, ids: &[PhotoId], c: &str) -> Result<(Vec<(PhotoId, std::sync::Arc<Vec<FoundFace>>)>, usize)> {
    let (mut ready, mut pending) = (Vec::new(), 0usize);
    for id in ids {
        match s.photo_appearance(*id, false).map_err(|e| bad(c, e))? {
            Some(f) => ready.push((*id, f)),
            None => pending += 1,
        }
    }
    Ok((ready, pending))
}

#[cfg(feature = "sam")]
mod look {
    use super::*;
    use lcc_segment::identity::{Signature, similarity};

    /// Everyone remembered, with their examples as in photo `id`'s shoot: those of its folder, or,
    /// when nobody was named there, every example.
    pub fn known(s: &mut Session, id: PhotoId) -> Vec<(String, Vec<Signature>)> {
        let folder = s.photo_context(id).folder;
        let store = s.people_store().clone();
        let parse = |e: &crate::people_store::Example| serde_json::from_value::<Signature>(e.signature.clone()).ok();
        let here: Vec<(String, Vec<Signature>)> = store
            .people
            .iter()
            .map(|p| {
                let sigs = p
                    .examples
                    .iter()
                    .filter(|e| folder.is_some() && s.photo_context(PhotoId(e.photo)).folder == folder)
                    .filter_map(parse)
                    .collect::<Vec<_>>();
                (p.name.clone(), sigs)
            })
            .filter(|(_, v)| !v.is_empty())
            .collect();
        if !here.is_empty() {
            return here;
        }
        store
            .people
            .iter()
            .map(|p| (p.name.clone(), p.examples.iter().filter_map(parse).collect::<Vec<_>>()))
            .filter(|(_, v)| !v.is_empty())
            .collect()
    }

    /// Who the faces not in `skip` are: each person at most once, the most alike first.
    /// `(face index, index into known, score)`.
    pub fn assign(faces: &[FoundFace], skip: &[usize], known: &[(String, Vec<Signature>)], threshold: f32) -> Vec<(usize, usize, f32)> {
        let person_score = |sig: &Signature, sigs: &[Signature]| {
            let mut v: Vec<f32> = sigs.iter().map(|q| similarity(sig, q)).filter(|v| v.is_finite()).collect();
            v.sort_by(|a, b| b.total_cmp(a));
            let top: Vec<f32> = v.into_iter().take(2).collect();
            if top.is_empty() { 0.0 } else { top.iter().sum::<f32>() / top.len() as f32 }
        };
        let mut pairs: Vec<(usize, usize, f32)> = Vec::new();
        for (i, f) in faces.iter().enumerate().filter(|(i, _)| !skip.contains(i)) {
            let Ok(sig) = serde_json::from_value::<Signature>(f.signature.clone()) else { continue };
            for (k, (_, sigs)) in known.iter().enumerate() {
                let sc = person_score(&sig, sigs);
                if sc >= threshold {
                    pairs.push((i, k, sc));
                }
            }
        }
        pairs.sort_by(|a, b| b.2.total_cmp(&a.2));
        let mut out: Vec<(usize, usize, f32)> = Vec::new();
        for (i, k, sc) in pairs {
            if !out.iter().any(|(fi, fk, _)| *fi == i || *fk == k) {
                out.push((i, k, sc));
            }
        }
        out
    }
}

/// Name faces (`[(photo, face)]`) as `name`: their Face regions (made when missing) in one
/// commit, and their looks remembered. → faces named.
#[cfg(feature = "sam")]
fn name_faces(s: &mut Session, faces: &[(PhotoId, FoundFace)], name: &str) -> Result<usize> {
    let mut by_photo: std::collections::BTreeMap<u64, Vec<&FoundFace>> = Default::default();
    for (p, f) in faces {
        by_photo.entry(p.0).or_default().push(f);
    }
    let (mut ops, mut named, mut examples) = (Vec::new(), 0usize, Vec::new());
    for (photo, list) in by_photo {
        let id = PhotoId(photo);
        let Some(p) = s.catalog.photo(id) else { continue };
        let mut meta = p.meta.clone();
        for f in list {
            match region_of(&meta, f.rect) {
                Some(r) => {
                    if let Some(reg) = meta.regions.get_mut(r) {
                        reg.name = Some(name.to_string());
                        if reg.description.as_deref().is_some_and(|d| d.starts_with(crate::cmd::face_detect::MARK)) {
                            reg.description = None;
                        }
                    }
                }
                None => meta.regions.push(lcc_meta::Region {
                    rect: lcc_geom::Rect { x0: f.rect[0], y0: f.rect[1], x1: f.rect[2], y1: f.rect[3] },
                    kind: lcc_meta::RegionKind::Face,
                    name: Some(name.to_string()),
                    description: None,
                }),
            }
            named += 1;
            examples.push(crate::people_store::Example { photo, rect: f.rect, signature: f.signature.clone() });
        }
        ops.push(Op::SetMeta { id, meta: Box::new(meta) });
    }
    if ops.is_empty() {
        return Ok(0);
    }
    s.commit(if named == 1 { "Name Face" } else { "Name Faces" }, Op::Batch { ops })?;
    s.skip_auto_write = true;
    s.with_people(|st| examples.into_iter().for_each(|e| st.add_example(name, e))).map_err(crate::EngineError::Other)?;
    Ok(named)
}

#[cfg(not(feature = "sam"))]
fn no_ai(c: &str) -> crate::EngineError {
    bad(c, "telling people apart needs a build with the AI models")
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "people.recognize",
            "Recognize People",
            [],
            None,
            "{ids?, apply?: bool (false), threshold?: 0..1}: match the faces on the photos to the people you named (people.learn; by look: within a shoot clothes and hair count most): {suggestions: [{photo, face, name, score}], named, pending}. `apply` names the confident ones (one undo step); in the app `pending` photos are still being analysed (ask again)",
            always,
            |s, p| {
                let c = "people.recognize";
                let ids = s.targets(p)?;
                if ids.len() > 20_000 {
                    return Err(bad(c, "at most 20000 photos at a time"));
                }
                #[cfg(feature = "sam")]
                {
                    let threshold = p.get("threshold").and_then(Value::as_f64).map_or(SUGGEST, |t| t.clamp(0.3, 0.99) as f32);
                    if s.people_store().people.is_empty() {
                        return Err(bad(c, "name a few faces first and learn their looks (people.learn): recognition matches against them"));
                    }
                    let (ready, pending) = faces_of(s, &ids, c)?;
                    let mut suggestions = Vec::new();
                    let mut todo: Vec<(PhotoId, FoundFace, String)> = Vec::new();
                    for (id, faces) in &ready {
                        let meta = s.catalog.photo(*id).map(|p| p.meta.clone()).unwrap_or_default();
                        let named: Vec<usize> = (0..faces.len()).filter(|i| faces.get(*i).is_some_and(|f| is_named(&meta, f))).collect();
                        // a person already named on the photo isn't suggested for another face
                        let here: Vec<String> =
                            meta.regions.iter().filter(|r| r.kind == lcc_meta::RegionKind::Face).filter_map(|r| r.name.clone()).collect();
                        let known: Vec<_> =
                            look::known(s, *id).into_iter().filter(|(n, _)| !here.iter().any(|h| h.trim().eq_ignore_ascii_case(n.trim()))).collect();
                        for (i, k, score) in look::assign(faces, &named, &known, threshold) {
                            if let (Some((name, _)), Some(f)) = (known.get(k), faces.get(i)) {
                                suggestions.push(json!({"photo": id.0, "face": i, "name": name, "score": score}));
                                todo.push((*id, f.clone(), name.clone()));
                            }
                        }
                    }
                    let mut named = 0;
                    if bool_or(p, "apply", false) && !todo.is_empty() {
                        let names: std::collections::BTreeSet<String> = todo.iter().map(|t| t.2.clone()).collect();
                        let before = s.commits();
                        for n in names {
                            let list: Vec<(PhotoId, FoundFace)> = todo.iter().filter(|t| t.2 == n).map(|t| (t.0, t.1.clone())).collect();
                            named += name_faces(s, &list, &n)?;
                        }
                        let steps = s.commits().wrapping_sub(before) as usize;
                        s.merge_undo(steps, "Recognize People");
                    }
                    Ok(json!({"suggestions": suggestions, "named": named, "pending": pending}))
                }
                #[cfg(not(feature = "sam"))]
                {
                    let _ = (ids, p);
                    Err(no_ai(c))
                }
            }
        ),
        cmd!(
            query "people.cluster",
            "Group Unnamed Faces",
            [],
            None,
            "{ids?, threshold?: 0..1 (0.6)}: group the unnamed faces of the photos (a shoot) by look, for naming a group at once: {clusters: [{faces: [{photo, face, rect}]}], pending}",
            always,
            |s, p| {
                let c = "people.cluster";
                let ids = s.targets(p)?;
                if ids.len() > 20_000 {
                    return Err(bad(c, "at most 20000 photos at a time"));
                }
                #[cfg(feature = "sam")]
                {
                    use lcc_segment::identity::{MAX_CLUSTERED, Signature, cluster};
                    let threshold = p.get("threshold").and_then(Value::as_f64).map_or(0.6, |t| t.clamp(0.2, 0.99) as f32);
                    let (ready, pending) = faces_of(s, &ids, c)?;
                    let (mut refs, mut sigs) = (Vec::new(), Vec::new());
                    for (id, faces) in &ready {
                        let meta = s.catalog.photo(*id).map(|p| p.meta.clone()).unwrap_or_default();
                        for (i, f) in faces.iter().enumerate() {
                            if sigs.len() >= MAX_CLUSTERED || is_named(&meta, f) {
                                continue;
                            }
                            if let Ok(sig) = serde_json::from_value::<Signature>(f.signature.clone()) {
                                refs.push(json!({"photo": id.0, "face": i, "rect": f.rect}));
                                sigs.push(sig);
                            }
                        }
                    }
                    let clusters: Vec<Value> = cluster(&sigs, threshold)
                        .into_iter()
                        .map(|cl| json!({"faces": cl.iter().filter_map(|i| refs.get(*i).cloned()).collect::<Vec<_>>()}))
                        .collect();
                    Ok(json!({"clusters": clusters, "pending": pending}))
                }
                #[cfg(not(feature = "sam"))]
                {
                    let _ = (ids, p);
                    Err(no_ai(c))
                }
            }
        ),
    ]
}
