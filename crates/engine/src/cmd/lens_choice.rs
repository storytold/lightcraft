//! Choosing a photo's lens profile by hand (issue #1049): for a lens the file doesn't name, or names in a way the lens
//! database doesn't recognise, the user picks the profile from the database. The choice is kept as the photo's lens
//! correction ([`LensSource::Chosen`]): Reload from Disk and a database lookup leave it alone, and a render prefers it
//! to the file's own correction. The photo's Lens field takes the profile's name, so the Optics panel, the Lens column
//! and the Lens filter say which lens it is. One undo step.

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_develop::LensSource;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_active, has_selection};
use crate::{Result, Session};

/// The photo `id` asks about, else the active one.
fn photo_of(s: &Session, p: &Value, c: &str) -> Result<PhotoId> {
    match super::photo_id(p, c)? {
        Some(id) if s.catalog.photo(id).is_some() => Ok(id),
        Some(id) => Err(bad(c, format!("no such photo {}", id.0))),
        None => s.active().ok_or_else(|| bad(c, "no photo")),
    }
}

/// `lens.profiles`: the profiles the photo's camera can choose from, and the one it has.
fn profiles(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "lens.profiles";
    let id = photo_of(s, p, C)?;
    let ph = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?;
    let Some(db) = crate::lens_profiles::database() else {
        return Ok(json!({"profiles": [], "chosen": null, "database": false}));
    };
    let chosen = ph.embedded_lens.is_some_and(|l| l.source == LensSource::Chosen).then(|| ph.meta.lens.clone());
    Ok(json!({"profiles": crate::lens_profiles::choices(&db, &ph.meta.make, &ph.meta.camera), "chosen": chosen, "database": true}))
}

/// The ops that give photo `id` (and its virtual copies) the profile `name` from `db`, for its file as `info` (a fresh
/// probe) reads it: the chosen correction, the Lens field naming it, and Enable Lens Corrections on. `Err` says why
/// the photo can't have it.
pub(crate) fn ops_for(
    s: &Session,
    id: PhotoId,
    db: &lightcraft_lensdb::LensDb,
    name: &str,
    info: &crate::media::ProbeInfo,
) -> std::result::Result<Vec<Op>, String> {
    let ph = s.catalog.photo(id).ok_or("no such photo")?;
    if !ph.develops_raw() {
        return Err("not a raw".into());
    }
    let o = info.orientation.unwrap_or_default();
    let (w, h) = if o.swaps_axes() { (info.height, info.width) } else { (info.width, info.height) };
    let focal = info.meta.focal_mm.or(ph.meta.focal_mm).map(f64::from);
    let aperture = info.meta.aperture.or(ph.meta.aperture).map(f64::from);
    let lens =
        crate::lens_profiles::chosen(db, name, (&ph.meta.make, &ph.meta.camera), focal, aperture, (f64::from(w.max(1)), f64::from(h.max(1))), o)?;
    let mut ops = Vec::new();
    // the photo and its virtual copies share the file and the lens
    for f in std::iter::once(&**ph).chain(s.catalog.photos().filter(|c| c.copy_of == Some(id)).map(|c| &**c)) {
        ops.push(Op::SetEmbeddedLens { id: f.id, lens: Some(Box::new(lens)) });
        if f.meta.lens != name {
            let mut meta = f.meta.clone();
            meta.lens = name.to_string();
            ops.push(Op::SetMeta { id: f.id, meta: Box::new(meta) });
        }
        if !f.develop.optics.lens_profile {
            let mut d = (*f.develop).clone();
            d.optics.lens_profile = true;
            ops.push(Op::SetDevelop { id: f.id, settings: std::sync::Arc::new(d), label: "Lens Corrections".into(), edited: Some((s.clock)()) });
        }
    }
    Ok(ops)
}

/// `photo.chooseLensProfile`: every target raw gets the profile named `lens`, read for its own size, orientation and
/// focal length (its file's header is read, as Reload from Disk does).
fn choose(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.chooseLensProfile";
    let name = p
        .get("lens")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| bad(C, "`lens` names a profile from lens.profiles"))?;
    let db = crate::lens_profiles::database().ok_or_else(|| bad(C, "there is no lens database (Settings ▸ Lens Corrections)"))?;
    let probe = s.media.file_probe.clone().ok_or_else(|| bad(C, "this session can't read files"))?;
    let targets = s.targets(p)?;
    s.end_interaction()?;
    let mut ops = Vec::new();
    let mut chosen = Vec::new();
    let mut skipped = Vec::new();
    for id in targets {
        let path = match s.catalog.photo(id).map(|p| crate::cmd::missing::checked_path(p)) {
            Some(Some(path)) => path.to_string(),
            Some(None) => {
                skipped.push(json!({"id": id.0, "why": "its file isn't available"}));
                continue;
            }
            None => continue,
        };
        let got = match crate::import::probe_with(&probe, &[path]).pop() {
            Some(Ok(info)) => ops_for(s, id, &db, name, &info),
            Some(Err(e)) => Err(e),
            None => continue,
        };
        match got {
            Ok(mine) => {
                chosen.extend(mine.iter().filter_map(|op| match op {
                    Op::SetEmbeddedLens { id, .. } => Some(id.0),
                    _ => None,
                }));
                ops.extend(mine);
            }
            Err(why) => skipped.push(json!({"id": id.0, "why": why})),
        }
    }
    if !ops.is_empty() {
        s.commit("Choose Lens Profile", Op::Batch { ops })?;
        for id in &chosen {
            s.media.forget(PhotoId(*id));
        }
    }
    Ok(json!({"chosen": chosen, "skipped": skipped}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "lens.profiles",
            "Lens Profiles",
            [],
            None,
            "{id?} → {profiles: the lens database's profiles the photo's camera can be given by hand (photo.chooseLensProfile), for its mount when the database knows it; chosen: the profile chosen for the photo, or null; database: false without a lens database}",
            has_active,
            profiles
        ),
        cmd!(
            "photo.chooseLensProfile",
            "Choose Lens Profile",
            [],
            None,
            "{ids?, lens: a name from lens.profiles} — gives the photos (raws) that lens database profile, for a lens their files don't name or the database doesn't recognise (issue #1049): their Lens field takes its name, Enable Lens Corrections is switched on, and Reload from Disk or looking lens profiles up again keep it; each file's header is read for its size, orientation and focal length; one undo step → {chosen: [ids], skipped: [{id, why}]}",
            has_selection,
            choose
        ),
    ]
}
