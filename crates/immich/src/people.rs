//! IMM-PEOPLE, the pure part: Immich faces → the photo's face regions (the People view reads the
//! names on face regions), and names given in the app → Immich person renames and merges.
//!
//! Sources: own design; Immich's public API documentation for faces and people (plan/immich.md).
//!
//! A region from Immich carries `description = "immich:<personId>"` (or `immich:` for a face
//! nobody named): "from Immich", shown as a suggestion. Confirming it ([`confirm`]) appends
//! `:confirmed`; a re-import replaces unconfirmed Immich regions and leaves every other region
//! (XMP, the app's own detector, confirmed ones) alone, skipping an Immich face that overlaps one.

use std::collections::{BTreeMap, BTreeSet};

use dac_catalog::Photo;
use dac_geom::Rect;
use dac_meta::{Region, RegionKind};

use crate::types::{Face, Person};

pub const MARK: &str = "immich:";
const CONFIRMED: &str = ":confirmed";

/// The Immich person id of a region from Immich (`Some("")` for an unnamed face).
pub fn person_of(r: &Region) -> Option<&str> {
    let d = r.description.as_deref()?.strip_prefix(MARK)?;
    Some(d.strip_suffix(CONFIRMED).unwrap_or(d))
}

pub fn is_confirmed(r: &Region) -> bool {
    r.description.as_deref().is_some_and(|d| d.starts_with(MARK) && d.ends_with(CONFIRMED))
}

/// A face box in pixels → a normalized rect, or `None` for a degenerate or hostile box.
pub fn face_rect(f: &Face) -> Option<Rect> {
    let (w, h) = (f.image_width, f.image_height);
    if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0) {
        return None;
    }
    let c = |v: f64, m: f64| if v.is_finite() { (v / m).clamp(0.0, 1.0) } else { 0.0 };
    let (x0, y0, x1, y1) = (c(f.bounding_box_x1, w), c(f.bounding_box_y1, h), c(f.bounding_box_x2, w), c(f.bounding_box_y2, h));
    let r = Rect { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) };
    (r.x1 - r.x0 > 1e-4 && r.y1 - r.y0 > 1e-4).then_some(r)
}

fn iou(a: &Rect, b: &Rect) -> f64 {
    let ix = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let iy = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    let i = ix * iy;
    let u = (a.x1 - a.x0) * (a.y1 - a.y0) + (b.x1 - b.x0) * (b.y1 - b.y0) - i;
    if u > 0.0 { i / u } else { 0.0 }
}

/// What [`regions_with_faces`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceMerge {
    pub added: usize,
    pub removed: usize,
    /// Immich faces that overlap a region the photo already had.
    pub skipped: usize,
}

/// The photo's regions with Immich's `faces`: unconfirmed Immich regions are replaced; faces of
/// hidden people are left out (`include_hidden` false); names come from the face's person.
pub fn regions_with_faces(p: &Photo, faces: &[Face], include_hidden: bool) -> (Vec<Region>, FaceMerge) {
    let mut m = FaceMerge::default();
    let mut out: Vec<Region> = Vec::new();
    for r in &p.meta.regions {
        if person_of(r).is_some() && !is_confirmed(r) {
            m.removed += 1;
        } else {
            out.push(r.clone());
        }
    }
    let kept = out.len();
    for f in faces.iter().take(500) {
        if f.person.as_ref().is_some_and(|x| x.is_hidden) && !include_hidden {
            continue;
        }
        let Some(rect) = face_rect(f) else { continue };
        if out.iter().take(kept).any(|r| r.kind == RegionKind::Face && iou(&r.rect, &rect) > 0.5) {
            m.skipped += 1;
            continue;
        }
        let (pid, name) = match &f.person {
            Some(x) => (safe_id(&x.id), Some(x.name.trim().to_string()).filter(|n| !n.is_empty())),
            None => (String::new(), None),
        };
        out.push(Region { rect, kind: RegionKind::Face, name, description: Some(format!("{MARK}{pid}")) });
        m.added += 1;
    }
    // an unchanged face set is not a change
    let before: Vec<&Region> = p.meta.regions.iter().filter(|r| person_of(r).is_some() && !is_confirmed(r)).collect();
    let after: Vec<&Region> = out.iter().skip(kept).collect();
    if before == after {
        m.added = 0;
        m.removed = 0;
    }
    (out, m)
}

fn safe_id(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect()
}

/// Mark the photo's Immich regions as confirmed (names kept as they are now).
pub fn confirm(p: &Photo) -> Vec<Region> {
    p.meta
        .regions
        .iter()
        .map(|r| {
            let mut r = r.clone();
            if let Some(pid) = person_of(&r).map(str::to_string)
                && !is_confirmed(&r)
            {
                r.description = Some(format!("{MARK}{pid}{CONFIRMED}"));
            }
            r
        })
        .collect()
}

/// What pushing names back means: renames (`person id → name`) and merges (`into ← others`) of
/// Immich people whose regions now carry another name, or the same name as another person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamePush {
    pub renames: Vec<(String, String)>,
    pub merges: Vec<(String, Vec<String>)>,
}

/// From the names on Immich regions across `photos` and the server's `people`. A person whose
/// regions carry two different names is left alone (ambiguous).
pub fn names_to_push<'a>(photos: impl Iterator<Item = &'a Photo>, people: &[Person], merge: bool) -> NamePush {
    let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in photos {
        for r in &p.meta.regions {
            if let Some(pid) = person_of(r).filter(|x| !x.is_empty())
                && let Some(n) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty())
            {
                seen.entry(pid.to_string()).or_default().insert(n.to_string());
            }
        }
    }
    let current: BTreeMap<&str, &str> = people.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
    let mut out = NamePush::default();
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (pid, names) in &seen {
        let [name] = names.iter().collect::<Vec<_>>()[..] else { continue };
        let Some(cur) = current.get(pid.as_str()) else { continue };
        if cur.trim() != name {
            out.renames.push((pid.clone(), name.clone()));
        }
        by_name.entry(name.to_lowercase()).or_default().push(pid.clone());
    }
    if merge {
        // other people already called that on the server join too
        for p in people {
            let n = p.name.trim().to_lowercase();
            if let Some(v) = by_name.get_mut(&n)
                && !n.is_empty()
                && !v.contains(&p.id)
                && !seen.contains_key(&p.id)
            {
                v.push(p.id.clone());
            }
        }
        for ids in by_name.into_values().filter(|v| v.len() > 1) {
            let mut ids = ids;
            ids.sort();
            let into = ids.remove(0);
            out.merges.push((into, ids));
        }
    }
    out
}
