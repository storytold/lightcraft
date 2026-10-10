//! One-way metadata mapping Immich → catalog, used on import (plan/immich.md → Ratings and
//! favourites; IMM-SYNC adds the other direction in Phase 4).
//!
//! - rating 1–5 → stars; −1 → the reject flag (no stars); null → nothing.
//! - favourite → the pick flag (unless rejected).
//! - description → caption.
//! - tags (`parent/child`) → hierarchical keywords (`parent|child`).
//! - albums → the album names, for the caller to put the photo into.
//! - GPS → `meta.gps` when both coordinates are finite and in range.

use dac_catalog::{Flag, Photo};

use crate::types::Asset;

/// Apply what Immich knows about `a` to a photo being imported. Only fills what the asset has:
/// a photo's own metadata (read from the file) is kept when Immich has nothing.
pub fn apply(a: &Asset, p: &mut Photo) {
    let exif = a.exif_info.clone().unwrap_or_default();
    match exif.rating {
        Some(r @ 1..=5) => p.rating = r as u8,
        Some(-1) => {
            p.rating = 0;
            p.flag = Flag::Reject;
        }
        _ => {}
    }
    if a.is_favorite && p.flag != Flag::Reject {
        p.flag = Flag::Pick;
    }
    if let Some(d) = exif.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        p.meta.caption = d.to_string();
    }
    for t in &a.tags {
        let k = tag_to_keyword(&t.value);
        if !k.is_empty() && !p.meta.keywords.iter().any(|x| x.eq_ignore_ascii_case(&k)) {
            p.meta.keywords.push(k);
        }
    }
    if let (Some(lat), Some(lon)) = (exif.latitude, exif.longitude)
        && lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
    {
        p.meta.gps = Some((lat, lon));
    }
    for (field, v) in [(&mut p.meta.city, exif.city), (&mut p.meta.state, exif.state), (&mut p.meta.country, exif.country)] {
        if let Some(v) = v.filter(|v| !v.trim().is_empty())
            && field.is_empty()
        {
            *field = v;
        }
    }
    if p.captured.is_none() {
        p.captured = a.local_capture();
    }
}

/// The ops that bring `p` (already in the catalog) to what [`apply`] makes of it.
pub fn ops(a: &Asset, p: &Photo) -> Vec<dac_catalog::Op> {
    use dac_catalog::Op;
    let mut q = p.clone();
    apply(a, &mut q);
    let mut out = Vec::new();
    if q.rating != p.rating {
        out.push(Op::SetRating { id: p.id, rating: q.rating });
    }
    if q.flag != p.flag {
        out.push(Op::SetFlag { id: p.id, flag: q.flag });
    }
    if q.meta != p.meta {
        out.push(Op::SetMeta { id: p.id, meta: Box::new(q.meta) });
    }
    if q.captured != p.captured {
        out.push(Op::SetCaptured { id: p.id, captured: q.captured });
    }
    out
}

/// `parent/child` → `parent|child` (levels trimmed, empty levels dropped).
pub fn tag_to_keyword(tag: &str) -> String {
    tag.split('/').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("|")
}
