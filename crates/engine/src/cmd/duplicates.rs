//! Duplicates (`library.findDuplicates`, `library.resolveDuplicates`): list the library's duplicate photos in groups, at
//! two levels that need no AI model, and mark the copies the user does not keep Rejected.
//!
//! - **exact**: byte-identical files (the same content hash).
//! - **near**: the same picture saved again (resized, re-encoded, converted): the 8 x 8 brightness signature Find Similar
//!   uses, with a strict cut-off and the same shape (aspect ratio). Exact copies are in these groups too.
//!
//! Each group names the copy to keep (the user's own marks first, then raw, edits, pixels, focus). Nothing here deletes a
//! photo: rejected photos stay in the library until the user deletes them (Photo > Delete Rejected Photos).

use std::collections::BTreeMap;

use lightcraft_catalog::{Flag, MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_pipeline::cull::similarity;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};

/// Signature similarity from which two photos are "the same picture" (Find Similar's default is 0.8; the same picture
/// saved again measures 0.99 and more).
const NEAR_MIN: f64 = 0.985;
/// The least `similarity` accepted: below it look-alikes, not copies, would be grouped.
const NEAR_FLOOR: f64 = 0.95;
/// Aspect ratios of one picture saved again agree within this share.
const ASPECT_TOLERANCE: f64 = 0.02;

fn known_keys(p: &Value, allowed: &[&str], c: &str) -> Result<()> {
    let Some(k) = p.as_object().and_then(|o| o.keys().find(|k| !allowed.contains(&k.as_str()))) else { return Ok(()) };
    Err(bad(c, format!("unknown parameter `{k}`: use {}", allowed.join(", "))))
}

/// A photo a scan can look at: in the library, not a video, not a virtual copy (which is meant to share its file).
fn usable(p: &Photo) -> bool {
    p.in_library() && p.copy_of.is_none() && p.duration.is_none()
}

/// What byte-identical files share: their content hash (a demo photo, which has no file, its scene). A copy made with
/// Photo ▸ Duplicate keeps its own `<hash>:dup<id>` and so is not grouped with its original: the user made it on purpose.
fn exact_key(p: &Photo) -> Option<String> {
    match (&p.content_hash, &p.source) {
        (Some(h), _) => Some(h.clone()),
        (None, Source::Demo { scene }) => Some(format!("demo:{scene}")),
        (None, Source::File { .. }) => None,
    }
}

/// Higher is better to keep: the user's own marks first (pick, stars, never a reject), then what the file holds (raw,
/// edits, pixels), then focus.
fn score(p: &Photo) -> f64 {
    let pixels = f64::from(p.width) * f64::from(p.height);
    let mut s = 0.0;
    if p.flag == Flag::Reject {
        s -= 1e6;
    }
    if p.flag == Flag::Pick {
        s += 1e5;
    }
    s += f64::from(p.rating.min(5)) * 1e4;
    if p.kind == MediaKind::Raw && p.preview_only.is_none() {
        s += 5e3;
    }
    if p.edited.is_some() {
        s += 2e3;
    }
    s += (pixels.max(1.0).log2() * 50.0).min(1.5e3);
    s + f64::from(p.analysis.as_ref().map_or(0.0, |a| a.sharpness).clamp(0.0, 100.0))
}

/// Why `keep` is proposed over `other`, as ids the interface words.
fn reasons_over(keep: &Photo, other: &Photo) -> Vec<&'static str> {
    let mut out = Vec::new();
    if keep.flag == Flag::Pick && other.flag != Flag::Pick {
        out.push("picked");
    }
    if keep.rating > other.rating {
        out.push("rated");
    }
    let raw = |p: &Photo| p.kind == MediaKind::Raw && p.preview_only.is_none();
    if raw(keep) && !raw(other) {
        out.push("raw");
    }
    if keep.edited.is_some() && other.edited.is_none() {
        out.push("edited");
    }
    if u64::from(keep.width) * u64::from(keep.height) > u64::from(other.width) * u64::from(other.height) {
        out.push("larger");
    }
    let sharp = |p: &Photo| p.analysis.as_ref().map_or(0.0, |a| a.sharpness);
    if sharp(keep) > sharp(other) + 5.0 {
        out.push("sharper");
    }
    out
}

/// Union-find over `n` items.
struct Sets(Vec<usize>);

impl Sets {
    fn new(n: usize) -> Self {
        Sets((0..n).collect())
    }
    fn find(&mut self, mut i: usize) -> usize {
        while let Some(&parent) = self.0.get(i) {
            if parent == i {
                break;
            }
            let up = self.0.get(parent).copied().unwrap_or(parent);
            if let Some(slot) = self.0.get_mut(i) {
                *slot = up;
            }
            i = up;
        }
        i
    }
    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b
            && let Some(slot) = self.0.get_mut(a.max(b))
        {
            *slot = a.min(b);
        }
    }
}

/// A set of byte-identical photos, the one to keep first, and what the near level knows of the set.
struct Unit {
    /// Best to keep first.
    members: Vec<PhotoId>,
    signature: Option<[f32; 64]>,
    aspect: f64,
}

/// `ids` ordered best-to-keep first (ties: the lower id).
fn ranked(s: &Session, mut ids: Vec<PhotoId>) -> Vec<PhotoId> {
    let score_of = |id: &PhotoId| s.catalog.photo(*id).map_or(f64::MIN, |p| score(p));
    ids.sort_by(|a, b| score_of(b).total_cmp(&score_of(a)).then(a.cmp(b)));
    ids
}

/// The photos a scan looks at: the ones named, else every usable library photo.
fn scope(s: &Session, p: &Value, c: &str) -> Result<Vec<PhotoId>> {
    match super::photo_ids(p, c)? {
        None => Ok(s.catalog.photos().filter(|q| usable(q)).map(|q| q.id).collect()),
        Some(named) => {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for id in named {
                if !s.catalog.photo(id).is_some_and(|q| usable(q)) {
                    return Err(bad(c, format!("photo {} is not a library photo a scan can look at", id.0)));
                }
                // a photo named twice is one photo (it would otherwise be grouped with itself)
                if seen.insert(id) {
                    out.push(id);
                }
            }
            Ok(out)
        }
    }
}

/// A duplicate scan in steps. [`Scan::begin`] reads the parameters and groups the byte-identical photos (catalog only),
/// [`Scan::step`] reads look-alike signatures, then joins the look-alikes, both within a time budget (reading is the
/// slow part: every cache miss decodes a thumbnail; the join compares signatures and grows with the library) and
/// [`Scan::finish`] only assembles the answer of `library.findDuplicates`. The command runs every step at once; the Find
/// Duplicates dialog runs a few milliseconds of them per frame, so its window keeps answering, shows the progress and
/// can stop the scan.
pub struct Scan {
    near: bool,
    min: f64,
    scanned: usize,
    units: Vec<Unit>,
    /// The next unit whose signature is read (near level).
    next: usize,
    unreadable: Vec<u64>,
    /// Which units are one picture (near level; filled by the join phase of [`Scan::step`]).
    sets: Sets,
    /// The units with a signature, in sweep order (built when the join starts).
    order: Option<Vec<(f32, usize)>>,
    /// How many entries of `order` have been compared with the ones after them.
    joined: usize,
}

impl Scan {
    /// Check `p` (the parameters of `library.findDuplicates`) and group the byte-identical photos.
    pub fn begin(s: &Session, p: &Value) -> Result<Scan> {
        const C: &str = "library.findDuplicates";
        known_keys(p, &["level", "ids", "similarity"], C)?;
        let near = match p.get("level") {
            None | Some(Value::Null) => true,
            Some(Value::String(l)) if l == "near" => true,
            Some(Value::String(l)) if l == "exact" => false,
            Some(v) => return Err(bad(C, format!("`level` must be \"exact\" or \"near\" (got {v})"))),
        };
        let min = match p.get("similarity") {
            None | Some(Value::Null) => NEAR_MIN,
            Some(v) => v
                .as_f64()
                .filter(|x| (NEAR_FLOOR..=1.0).contains(x))
                .ok_or_else(|| bad(C, format!("`similarity` must be a number from {NEAR_FLOOR} to 1 (got {v})")))?,
        };
        let ids = scope(s, p, C)?;

        // exact: the same content hash
        let mut by_hash: BTreeMap<String, Vec<PhotoId>> = BTreeMap::new();
        let mut alone: Vec<PhotoId> = Vec::new();
        for &id in &ids {
            match s.catalog.photo(id).and_then(|q| exact_key(q)) {
                Some(key) => by_hash.entry(key).or_default().push(id),
                None => alone.push(id),
            }
        }
        let mut units: Vec<Unit> = Vec::new();
        for members in by_hash.into_values().chain(alone.into_iter().map(|id| vec![id])) {
            units.push(Unit { members: ranked(s, members), signature: None, aspect: 0.0 });
        }
        units.sort_by_key(|u| u.members.iter().map(|m| m.0).min());
        let sets = Sets::new(units.len());
        Ok(Scan { near, min, scanned: ids.len(), units, next: 0, unreadable: Vec::new(), sets, order: None, joined: 0 })
    }

    /// `(units looked at, units)`: how far the signatures are read (the exact level reads none).
    pub fn progress(&self) -> (usize, usize) {
        (if self.near { self.next } else { self.units.len() }, self.units.len())
    }

    /// Work for about `budget` (at least one unit per call; all that is left without a budget). Near level: first one
    /// signature per unit, read from its best copy, then the join of the look-alikes, one unit at a time. `true` when
    /// nothing is left to do.
    pub fn step(&mut self, s: &mut Session, budget: Option<std::time::Duration>) -> bool {
        if !self.near {
            return true;
        }
        let deadline = budget.and_then(|b| web_time::Instant::now().checked_add(b));
        loop {
            if self.next < self.units.len() {
                self.read_signature(s);
                self.next += 1;
            } else if self.join_step() {
                return true;
            }
            if deadline.is_some_and(|d| web_time::Instant::now() >= d) {
                return false;
            }
        }
    }

    /// Read the signature of unit `self.next`.
    fn read_signature(&mut self, s: &mut Session) {
        let Some(u) = self.units.get_mut(self.next) else { return };
        let Some(&best) = u.members.first() else { return };
        let (w, h) = s.catalog.photo(best).map_or((0, 0), |q| (q.width, q.height));
        u.aspect = if w > 0 && h > 0 { f64::from(w) / f64::from(h) } else { 0.0 };
        if u.aspect > 0.0 {
            u.signature = super::cull::signature_of(s, best).filter(|sig| sig.iter().any(|v| *v != 0.0));
            if u.signature.is_none() {
                self.unreadable.extend(u.members.iter().map(|m| m.0));
            }
        }
    }

    /// Compare the next unit of the sweep with the ones close to it and join the look-alikes. `true` when the sweep is
    /// through. Units are swept in order of their signature's position along one fixed direction: two signatures that
    /// are `min` alike are no further apart than `sqrt(2 - 2 min)` there, so a unit is only compared with the ones that
    /// close.
    fn join_step(&mut self) -> bool {
        let units = &self.units;
        let order = self.order.get_or_insert_with(|| {
            let weights = sweep_weights();
            let norm = sweep_norm(&weights);
            let mut order: Vec<(f32, usize)> =
                units.iter().enumerate().filter_map(|(i, u)| Some((along(u.signature.as_ref()?, &weights, norm), i))).collect();
            order.sort_by(|a, b| a.0.total_cmp(&b.0));
            order
        });
        let n = self.joined;
        let Some(&(pos, i)) = order.get(n) else { return true };
        self.joined = n.saturating_add(1);
        let min = self.min as f32;
        let reach = (2.0 - 2.0 * min).max(0.0).sqrt() + 1e-4;
        if let Some(a) = units.get(i) {
            for &(other, j) in order.iter().skip(n.saturating_add(1)) {
                if other - pos > reach {
                    break;
                }
                let Some(b) = units.get(j) else { continue };
                let (Some(sa), Some(sb)) = (&a.signature, &b.signature) else { continue };
                let shape = (a.aspect - b.aspect).abs() <= ASPECT_TOLERANCE * a.aspect.max(b.aspect);
                if shape && similarity(sa, sb) >= min {
                    self.sets.join(i, j);
                }
            }
        }
        self.joined >= order.len()
    }

    /// The answer of `library.findDuplicates`, once [`Scan::step`] has returned `true`.
    pub fn finish(self, s: &Session) -> Value {
        let Scan { near, scanned, units, unreadable, mut sets, .. } = self;

        // groups: the units that ended in one set
        let mut sets_members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..units.len() {
            sets_members.entry(sets.find(i)).or_default().push(i);
        }
        let mut groups: Vec<(bool, u64, Value)> = Vec::new();
        for unit_ids in sets_members.into_values() {
            let members: Vec<PhotoId> = unit_ids.iter().filter_map(|&i| units.get(i)).flat_map(|u| u.members.iter().copied()).collect();
            if members.len() < 2 {
                continue;
            }
            let all = ranked(s, members);
            let Some(&keep) = all.first() else { continue };
            let Some(keep_unit) = unit_ids.iter().filter_map(|&i| units.get(i)).find(|u| u.members.contains(&keep)) else { continue };
            let exact_level = unit_ids.len() == 1;
            let photos: Vec<Value> = all
                .iter()
                .filter_map(|&id| {
                    let q = s.catalog.photo(id)?;
                    let unit = unit_ids.iter().filter_map(|&i| units.get(i)).find(|u| u.members.contains(&id))?;
                    let same_file = std::ptr::eq(unit, keep_unit);
                    let alike = match (same_file, &unit.signature, &keep_unit.signature) {
                        (true, ..) => 1.0,
                        (false, Some(a), Some(b)) => similarity(a, b),
                        _ => 0.0,
                    };
                    Some(json!({
                        "id": id.0,
                        "file": q.file_name,
                        "width": q.width,
                        "height": q.height,
                        "exact": same_file,
                        "similarity": (f64::from(alike) * 1000.0).round() / 1000.0,
                        "reasons": if id == keep { Vec::new() } else { s.catalog.photo(keep).map(|k| reasons_over(k, q)).unwrap_or_default() },
                    }))
                })
                .collect();
            let mut reasons: Vec<&str> = Vec::new();
            for r in photos.iter().skip(1).filter_map(|p| p["reasons"].as_array()).flatten().filter_map(Value::as_str) {
                if !reasons.contains(&r) {
                    reasons.push(r);
                }
            }
            let first = all.iter().map(|i| i.0).min().unwrap_or(0);
            groups.push((
                !exact_level,
                first,
                json!({"level": if exact_level { "exact" } else { "near" }, "keep": keep.0, "reasons": reasons, "photos": photos}),
            ));
        }
        // exact groups first, then the near ones, each by its lowest photo id
        groups.sort_by_key(|g| (g.0, g.1));
        let in_groups: usize = groups.iter().map(|g| g.2["photos"].as_array().map_or(0, Vec::len)).sum();
        let groups: Vec<Value> = groups.into_iter().map(|g| g.2).collect();
        json!({"level": if near { "near" } else { "exact" }, "scanned": scanned, "groups": groups, "photosInGroups": in_groups, "unreadable": unreadable})
    }
}

fn find(s: &mut Session, p: &Value) -> Result<Value> {
    let mut scan = Scan::begin(s, p)?;
    scan.step(s, None);
    Ok(scan.finish(s))
}

/// The fixed direction the join sweeps along.
fn sweep_weights() -> [f32; 64] {
    std::array::from_fn(|i| (i % 8) as f32 - 3.5)
}

fn sweep_norm(weights: &[f32; 64]) -> f32 {
    weights.iter().map(|w| w * w).sum::<f32>().sqrt().max(1e-6)
}

/// A signature's position along the sweep direction.
fn along(sig: &[f32; 64], weights: &[f32; 64], norm: f32) -> f32 {
    sig.iter().zip(weights).map(|(v, w)| v * w).sum::<f32>() / norm
}

/// Whether `group` is one group of the same picture at the loosest cut-off.
fn is_one_near_group(s: &mut Session, group: &[PhotoId]) -> Result<bool> {
    let ids: Vec<u64> = group.iter().map(|id| id.0).collect();
    let found = find(s, &json!({"level": "near", "ids": ids, "similarity": NEAR_FLOOR}))?;
    let groups = found["groups"].as_array().map_or(&[][..], Vec::as_slice);
    Ok(groups.len() == 1 && groups.first().and_then(|g| g["photos"].as_array()).is_some_and(|p| p.len() == group.len()))
}

/// Mark every photo of a group Rejected except the ones kept. One undo step; nothing is deleted.
fn resolve(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.resolveDuplicates";
    known_keys(p, &["ids", "keep"], C)?;
    let ids = super::photo_ids(p, C)?.ok_or_else(|| bad(C, "missing `ids`: the photos of the group (from library.findDuplicates)"))?;
    let keep = match p.get("keep") {
        None | Some(Value::Null) => return Err(bad(C, "missing `keep`: the photo ids of the group to keep (at least one)")),
        Some(Value::Array(a)) => {
            a.iter().map(|v| v.as_u64().map(PhotoId).ok_or_else(|| bad(C, "`keep` must be photo ids (whole numbers)"))).collect::<Result<Vec<_>>>()?
        }
        Some(_) => return Err(bad(C, "`keep` must be a list of photo ids")),
    };
    if keep.is_empty() {
        return Err(bad(C, "keep at least one photo of the group: nothing is rejected without a photo kept"));
    }
    let mut group: Vec<PhotoId> = Vec::new();
    for id in ids {
        if !s.catalog.photo(id).is_some_and(|q| usable(q)) {
            return Err(bad(C, format!("photo {} is not a library photo", id.0)));
        }
        if !group.contains(&id) {
            group.push(id);
        }
    }
    if group.len() < 2 {
        return Err(bad(C, "a group has at least two photos"));
    }
    if let Some(x) = keep.iter().find(|k| !group.contains(k)) {
        return Err(bad(C, format!("photo {} is not in `ids`", x.0)));
    }
    // a caller other than the dialog can name any photos: they must be one group. A group found at a stricter cut-off is
    // still one group of its own photos at the loosest one, so the dialog's groups always pass. Byte-identical photos
    // pass on their content hash alone; a near group's signatures were read by the scan that found it (cached by
    // content), so this decodes nothing and compares only the group's own photos.
    let mut keys = group.iter().map(|id| s.catalog.photo(*id).and_then(|q| exact_key(q)));
    let first = keys.next().flatten();
    let one_file = first.is_some() && keys.all(|k| k == first);
    if !one_file && !is_one_near_group(s, &group)? {
        return Err(bad(C, "these photos are not one group of duplicates: use a group from library.findDuplicates"));
    }
    let mut ops = Vec::new();
    let (mut rejected, mut kept) = (Vec::new(), Vec::new());
    for id in &group {
        if keep.contains(id) {
            kept.push(id.0);
        } else if s.catalog.photo(*id).is_some_and(|q| q.flag != Flag::Reject) {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Reject });
            rejected.push(id.0);
        }
    }
    if !ops.is_empty() {
        s.commit("Mark Duplicates Rejected", Op::Batch { ops })?;
    }
    Ok(json!({"rejected": rejected, "kept": kept}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "library.findDuplicates",
            "Find Duplicates",
            [],
            None,
            "{level?: exact | near (default), ids?: photo ids (default: every library photo but videos and virtual copies), similarity?: 0.95..1 (0.985; near only)} — group the duplicate photos, no AI model: exact = byte-identical files (same content hash); near = also the same picture saved again (resized, re-encoded), by Find Similar's 8x8 brightness signature at a strict cut-off and the same shape. Slow on a big library the first time (every photo's signature is read once; they are cached) → {level, scanned, photosInGroups, unreadable: [ids], groups: [{level: exact | near, keep: the copy to keep (the user's pick, stars, a raw file, edits, more pixels, sharper), reasons: [picked | rated | raw | edited | larger | sharper], photos: [{id, file, width, height, exact: byte-identical to the keeper, similarity, reasons}]}]}",
            always,
            find
        ),
        cmd!(
            "library.resolveDuplicates",
            "Mark Others Rejected",
            [],
            None,
            "{ids: the photos of one group, keep: [the ids to keep, at least one of them]} — mark the group's other photos Rejected (one undo step); never deletes a photo (Delete Rejected Photos does that, when the user asks) → {rejected, kept}",
            always,
            resolve
        ),
    ]
}
