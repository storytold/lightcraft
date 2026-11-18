//! Flat-field correction (Capture One's LCC "Uniform Light") as commands: measure a map from a reference raw, list the
//! maps that could serve a photo, apply one to photos, remove it, and what the Optics panel shows (design notes §10).
//! The catalog stores each map once ([`lcc_catalog::vignetting_maps`]); the library in the settings folder keeps every map
//! made, for any catalog ([`crate::vignetting_calibration_library`]).

use lcc_catalog::{Op, PhotoId, VignettingMapRecord};
use lcc_develop::{EmbeddedVignettingMap, VignettingMapId};
use lcc_raw::vignetting_plausibility::{self as vp, Plausibility, Reason, Verdict};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_active, has_selection, str_param};
use crate::vignetting_calibration_library::{self as lib, Entry, MatchReport};
use crate::{Result, Session};

/// The photo a command is about: `id`, else the active photo.
fn photo_arg(s: &Session, p: &Value, c: &str) -> Result<PhotoId> {
    match p.get("id").and_then(Value::as_u64) {
        Some(id) => s.catalog.photo(PhotoId(id)).map(|p| p.id).ok_or_else(|| bad(c, format!("no photo {id}"))),
        None if p.get("id").is_some_and(|v| !v.is_null()) => Err(bad(c, "id must be a photo id")),
        None => s.active().ok_or_else(|| bad(c, "no photo")),
    }
}

/// A map's facts for the UI and agents.
fn map_json(id: &VignettingMapId, m: &EmbeddedVignettingMap) -> Value {
    let s = &m.source;
    json!({
        "id": id.as_str(),
        "model": s.model, "serial": s.serial, "lens": s.lens,
        "focal": s.focal_length, "aperture": s.aperture,
        "fileName": s.file_name, "captured": s.captured,
        "grid": m.grid, "sensor": m.sensor,
    })
}

fn report_json(r: &MatchReport) -> Value {
    serde_json::to_value(r).unwrap_or(Value::Null)
}

/// How a reference was exposed, from its raw samples: levels as a share of the raw's white.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exposure {
    /// R, G, B in the middle of the frame (5 % of the short side across).
    pub centre: [f64; 3],
    /// Green in the darkest part of the frame (the centre's level less the measured fall-off).
    pub darkest: f64,
    /// Share of all samples at or above the clip level.
    pub clipped: f64,
}

/// Below this green level in the middle the reference is underexposed (noise shows in the corners): about a stop under
/// a camera histogram's peak at 2/3 of its width.
pub const UNDER_LEVEL: f64 = 0.25;
/// Above this level in the middle (any channel) a brighter part of the frame or another light could clip.
pub const NEAR_CLIP_LEVEL: f64 = 0.85;

impl Exposure {
    /// "under", "nearClip" or "ok".
    pub fn verdict(&self) -> &'static str {
        if self.centre[1] < UNDER_LEVEL {
            "under"
        } else if self.centre.iter().any(|c| *c > NEAR_CLIP_LEVEL) {
            "nearClip"
        } else {
            "ok"
        }
    }

    fn json(&self) -> Value {
        json!({"centre": self.centre, "darkest": self.darkest, "clipped": self.clipped, "verdict": self.verdict()})
    }
}

/// The levels of a decoded reference (`clip`: the builder's clip level).
fn exposure_of(n: &lcc_raw::Normalized, clip: f32) -> Exposure {
    let (w, h) = (n.width, n.height);
    let half = (w.min(h) / 40).max(2);
    let (cx, cy) = (w / 2, h / 2);
    let (mut sum, mut count) = ([0f64; 3], [0usize; 3]);
    for y in cy.saturating_sub(half)..(cy + half).min(h) {
        for x in cx.saturating_sub(half)..(cx + half).min(w) {
            for (c, v) in samples_at(n, x, y) {
                if v.is_finite() {
                    sum[c] += f64::from(v);
                    count[c] += 1;
                }
            }
        }
    }
    let centre = [0, 1, 2].map(|c| if count[c] > 0 { sum[c] / count[c] as f64 } else { 0.0 });
    // (every 7th sample is plenty for a share)
    let (mut clipped, mut seen) = (0usize, 0usize);
    for v in n.data.iter().step_by(7) {
        seen += 1;
        clipped += usize::from(*v >= clip);
    }
    Exposure { centre, darkest: centre[1], clipped: clipped as f64 / seen.max(1) as f64 }
}

/// The samples at `(x, y)` with their colour (0 R, 1 G, 2 B): one for a mosaic, three for RGB.
fn samples_at(n: &lcc_raw::Normalized, x: usize, y: usize) -> Vec<(usize, f32)> {
    let i = (y * n.width + x) * n.cpp;
    match (n.cpp, n.cfa.as_ref()) {
        (1, Some(cfa)) => {
            let c = match cfa.color_at(x, y) {
                0 => 0,
                2 => 2,
                _ => 1,
            };
            n.data.get(i).map(|v| vec![(c, *v)]).unwrap_or_default()
        }
        (1, None) => n.data.get(i).map(|v| vec![(1, *v)]).unwrap_or_default(),
        _ => (0..3).filter_map(|c| n.data.get(i + c).map(|v| (c, *v))).collect(),
    }
}

/// A reference measured into a map, ready to keep and apply ([`commit_measured`]). Made without the session, so the
/// app measures on a worker thread while the window keeps drawing.
#[derive(Clone, Debug)]
pub struct Measured {
    pub map: EmbeddedVignettingMap,
    pub sha1: String,
    /// EV at the darkest cell.
    pub falloff: f64,
    pub uneven: bool,
    centre: Value,
    pub exposure: Exposure,
    /// Whether the raw looks like a flat-field reference ([`lcc_raw::vignetting_plausibility`]): never Fail here
    /// ([`measure`] refuses those); a Borderline one is kept only when asked to ([`commit_measured`]'s `force`).
    pub plausibility: Plausibility,
    /// Whether vibration reduction (in-body or lens) was on for the shot, as the raw records it (`None`: not recorded).
    /// On is allowed, with a note: the stabiliser can shift the image slightly and shapes the fall-off less precisely.
    pub vr: Option<bool>,
}

impl Measured {
    /// A measured reference made from a map, as if its raw had been measured (the app's tests: a Create Anyway
    /// without a raw file).
    #[doc(hidden)]
    pub fn for_tests(map: EmbeddedVignettingMap, plausibility: Plausibility) -> Measured {
        let sha1 = map.source.sha1.clone().unwrap_or_default();
        let exposure = Exposure { centre: [0.4; 3], darkest: 0.2, clipped: 0.0 };
        Measured { map, sha1, falloff: 1.0, uneven: false, centre: Value::Null, exposure, plausibility, vr: None }
    }
}

/// Why [`measure`] made no map.
#[derive(Clone, Debug)]
pub enum MeasureError {
    /// The raw doesn't look like a flat-field reference: detail, colours or a light that isn't a lens's fall-off.
    Implausible(Box<Plausibility>),
    /// It couldn't be read or measured.
    Other(String),
}

impl std::fmt::Display for MeasureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeasureError::Implausible(p) => f.write_str(&refusal_text(p)),
            MeasureError::Other(e) => f.write_str(e),
        }
    }
}

impl From<String> for MeasureError {
    fn from(e: String) -> Self {
        MeasureError::Other(e)
    }
}

impl From<&str> for MeasureError {
    fn from(e: &str) -> Self {
        MeasureError::Other(e.to_string())
    }
}

/// One reason in words (the commands' English; the app shows its own translated sentences).
pub fn reason_text(r: Reason) -> &'static str {
    match r {
        Reason::Detail => "it has detail in it (a flat-field reference shows only an even surface)",
        Reason::Colour => "its colours aren't even",
        Reason::Shape => "it doesn't get darker evenly from the middle toward the edges",
        Reason::NoFalloff => "it doesn't get darker toward the edges",
        Reason::Dark => "it is too dark",
        Reason::Clipped => "parts of it are clipped",
    }
}

/// Why a raw was refused or held back, with the telling measurement.
fn refusal_text(p: &Plausibility) -> String {
    let why: Vec<&str> = p.reasons.iter().map(|r| reason_text(*r)).collect();
    format!(
        "This photo doesn't look like a flat-field reference: {} (largest local deviation {:.3} EV; a flat field stays under {:.2} EV)",
        why.join("; "),
        p.metrics.detail_max,
        vp::DETAIL_MAX.borderline
    )
}

/// Measure the raw at `path` (the photo's `file_name` and `captured` go into the map's source) as a vignetting reference.
/// A raw that clearly isn't a flat-field reference is refused ([`MeasureError::Implausible`]) before a map is made.
pub fn measure(path: &str, file_name: &str, captured: Option<String>) -> std::result::Result<Measured, MeasureError> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut sha = lcc_hash::Sha1::new();
    sha.update(&bytes);
    let sha1 = sha.finish().to_hex();
    let raw = lcc_raw::decode(&bytes).map_err(|e| e.to_string())?;
    let meta = {
        let mut m = lcc_meta::extract(&bytes);
        let r = &raw.metadata;
        m.make = m.make.or_else(|| r.make.clone());
        m.model = m.model.or_else(|| r.model.clone());
        m.serial_number = m.serial_number.or_else(|| r.serial_number.clone());
        m.lens_make = m.lens_make.or_else(|| r.lens_make.clone());
        m.lens_model = m.lens_model.or_else(|| r.lens_model.clone());
        m.focal_length = m.focal_length.or(r.focal_length);
        m.f_number = m.f_number.or(r.f_number);
        m
    };
    let n = raw.normalized().map_err(|e| e.to_string())?;
    // a landscape, a portrait, a sky with a bird: refused before a map is made of it
    let plausibility = vp::check(&n).ok_or("the raw has no image to measure")?;
    if plausibility.verdict == Verdict::Fail {
        return Err(MeasureError::Implausible(Box::new(plausibility)));
    }
    let window = lcc_raw::sensor_window(&bytes, [n.width, n.height]);
    drop(bytes);
    let source = lcc_raw::vignetting_calibration::VignettingCalibrationSource::from_metadata(&meta, Some(sha1.clone()));
    let opts = lcc_raw::vignetting_calibration::VignettingCalibrationOptions::default();
    let mut exposure = exposure_of(&n, opts.clip);
    let mut measured = lcc_raw::vignetting_calibration::build(&n, source, &opts).map_err(|e| e.to_string())?;
    drop(n);
    measured.window = window;
    let grid = measured.ev_grid();
    let centre = lcc_raw::vignetting_calibration::fit_centre(&grid);
    let falloff = grid.ev.iter().copied().fold(0.0f64, f64::max);
    exposure.darkest = exposure.centre[1] * (-falloff).exp2();
    let mut map = lcc_pipeline::vignetting_calibration::embed(&measured).ok_or("the map doesn't fit the develop settings")?;
    map.source.file_name = Some(file_name.to_string());
    map.source.captured = captured;
    let uneven = lib::uneven(&map);
    let centre = centre.map_or(Value::Null, |c| json!({"dx": c.dx_pct, "dy": c.dy_pct, "rms": c.rms_ev}));
    let vr = meta.stabilization;
    Ok(Measured { map, sha1, falloff, uneven, centre, exposure, plausibility, vr })
}

/// The references in use of the same setup as `m` ([`lib::same_setup`]: camera model, lens, aperture, a zoom's focal
/// length), in this catalog or the library, other than `m` itself, that `m` covers ([`lib::covers`]: the same image
/// area, or one inside it): what Create would replace. One that covers more (an FX reference, for a new DX one) or
/// another area stays in use for the shots `m` doesn't cover ([`kept_beside`]).
pub fn conflicts(s: &Session, m: &Measured) -> Vec<Entry> {
    let own = VignettingMapId::of(&m.map);
    pool(s).into_iter().filter(|e| !e.withdrawn && e.id != own && lib::same_setup(&e.map, &m.map) && lib::covers(&m.map, &e.map)).collect()
}

/// The references in use of the same setup as `m` that it doesn't cover: they stay in use beside it, each for its own
/// image area (Apply uses the tightest that covers a photo).
pub fn kept_beside(s: &Session, m: &Measured) -> Vec<Entry> {
    let own = VignettingMapId::of(&m.map);
    pool(s).into_iter().filter(|e| !e.withdrawn && e.id != own && lib::same_setup(&e.map, &m.map) && !lib::covers(&m.map, &e.map)).collect()
}

/// The question Create asks before replacing `existing` (the commands' English; the app asks in its own words).
fn replace_text(existing: &Entry, new: &EmbeddedVignettingMap) -> String {
    let s = &existing.map.source;
    let wider = if lib::coverage(&existing.map) == lib::coverage(new) { "" } else { " (the new one covers its image area too)" };
    format!(
        "There's already a reference for {} on {} (from {}, {}){wider}; create with replace: true to replace it",
        combination_text(&existing.map),
        s.model.as_deref().unwrap_or("this camera"),
        s.file_name.as_deref().unwrap_or("?"),
        s.captured.as_deref().map(|c| c.get(..10).unwrap_or(c)).unwrap_or("?")
    )
}

/// A reference's lens and aperture in words: "NIKKOR Z 50mm f/1.8 S at f/1.8", a zoom with its focal length.
pub fn combination_text(map: &EmbeddedVignettingMap) -> String {
    let s = &map.source;
    let mut lens = s.lens.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| "an unnamed lens".into());
    if let (true, Some(f)) = (s.lens.as_deref().is_some_and(lib::is_zoom), s.focal_length) {
        lens = format!("{lens} at {} mm", f.round());
    }
    match s.aperture {
        Some(a) => format!("{lens} at f/{}", aperture_text(a)),
        None => lens,
    }
}

/// An f-number as cameras show it: 1.8, 4, 6.3, 13.
pub fn aperture_text(a: f64) -> String {
    let r = (a * 10.0).round() / 10.0;
    if (r - r.round()).abs() < 1e-9 { format!("{}", r.round()) } else { format!("{r:.1}") }
}

/// Keep a measured map (catalog and library) and apply it to reference photo `id` at amount 100, as one undo step
/// ("Vignetting Calibration" in the History). A Borderline reference is kept only with `force` (Create Anyway). Only
/// one reference is in use per combination ([`conflicts`]): with one already, it is replaced only with `replace` (it is
/// withdrawn in the same undo step; photos that use it keep it). A map the catalog holds already (the same reference
/// measured again) is put back in use if it was withdrawn.
pub fn commit_measured(s: &mut Session, id: PhotoId, m: Measured, force: bool, replace: bool) -> Result<Value> {
    const C: &str = "vignettingCalibration.create";
    if m.plausibility.verdict == Verdict::Borderline && !force {
        return Err(bad(C, format!("{}; create it anyway with force: true", refusal_text(&m.plausibility))));
    }
    let replaced = conflicts(s, &m);
    if let Some(first) = replaced.first().filter(|_| !replace) {
        return Err(bad(C, replace_text(first, &m.map)));
    }
    let kept = kept_beside(s, &m);
    let photo = s.catalog.photo(id).cloned().ok_or_else(|| bad(C, "the reference photo is no longer in this catalog"))?;
    let map_id = VignettingMapId::of(&m.map);
    // the library keeps it for every catalog (none in the web build)
    let entry = s.vignetting_calibration_dir().map(|d| lib::save(&d, &m.map)).transpose().map_err(|e| bad(C, e))?;
    let now = (s.clock)();
    let mut ops = Vec::new();
    if photo.sha1.is_none() {
        ops.push(Op::SetSha1 { id, sha1: Some(m.sha1.clone()) });
    }
    // the reference it replaces: withdrawn in this step (one only in the library joins the catalog first, in use, so
    // undo puts it back in use here and in its library file)
    for old in &replaced {
        if s.catalog.vignetting_map(&old.id).is_none() {
            let record = VignettingMapRecord { map: old.map.clone(), photo: None, added: now.clone(), withdrawn: false };
            ops.push(Op::AddVignettingMap { id: old.id.clone(), record: Box::new(record) });
        }
        ops.push(Op::SetVignettingMapState { id: old.id.clone(), withdrawn: true });
    }
    // added withdrawn, then put in use: undoing the Create withdraws the map (it stays, for the photos that use it),
    // so Apply doesn't pick an undone reference as the newest; a map the catalog holds already is put back in use
    let record = VignettingMapRecord { map: m.map.clone(), photo: Some(id), added: now, withdrawn: true };
    ops.push(Op::AddVignettingMap { id: map_id.clone(), record: Box::new(record) });
    ops.push(Op::SetVignettingMapState { id: map_id.clone(), withdrawn: false });
    let mut d = (*photo.develop).clone();
    d.optics.vignetting_calibration = Some(map_id.clone());
    d.optics.vignetting_calibration_amount = lcc_develop::DEFAULT_VIGNETTING_CALIBRATION_PERCENT;
    ops.extend(s.develop_op(id, d, "Vignetting Calibration"));
    s.commit("Vignetting Calibration", Op::Batch { ops })?;
    Ok(json!({
        "map": map_json(&map_id, &m.map),
        "key": entry.map(|e| e.key),
        "falloff": m.falloff,
        "uneven": m.uneven,
        "centre": m.centre,
        "exposure": m.exposure.json(),
        "plausibility": serde_json::to_value(&m.plausibility).unwrap_or(Value::Null),
        "vr": m.vr,
        "replaced": replaced.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        "keptForOtherAreas": kept.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
    }))
}

/// The reference photo `id` as Create needs it: a raw file on disk (its path, file name and capture date).
pub fn reference_file(s: &Session, id: PhotoId) -> Result<(String, String, Option<String>)> {
    const C: &str = "vignettingCalibration.create";
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "no photo"))?;
    let lcc_catalog::Source::File { path } = &photo.source else { return Err(bad(C, "not a file on disk")) };
    if !photo.develops_raw() {
        return Err(bad(C, "a vignetting reference has to be a raw file"));
    }
    Ok((path.clone(), photo.file_name.clone(), photo.captured.clone()))
}

/// `vignettingCalibration.create`: measure the photo's raw as a vignetting reference, keep the map (catalog and library) and apply it to
/// the photo at amount 100, as one undo step. (The app measures on a worker thread: [`measure`], then
/// [`commit_measured`].)
fn create(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "vignettingCalibration.create";
    let id = photo_arg(s, p, C)?;
    let (path, file_name, captured) = reference_file(s, id)?;
    let task = s.activity.start("vignettingCalibration", "Measuring the vignetting reference", crate::activity::Cancel::No);
    let flag = |key: &str| match p.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(C, format!("{key} must be true or false"))),
    };
    let (force, replace) = (flag("force")?, flag("replace")?);
    let m = measure(&path, &file_name, captured).map_err(|e| bad(C, e.to_string()))?;
    drop(task);
    commit_measured(s, id, m, force, replace)
}

/// Every map that could serve a photo: the library's and the catalog's own, merged by id (read once for a whole
/// selection).
fn pool(s: &Session) -> Vec<Entry> {
    let mut by_id: std::collections::BTreeMap<VignettingMapId, Entry> = std::collections::BTreeMap::new();
    if let Some(d) = s.vignetting_calibration_dir() {
        for e in lib::list(&d, None) {
            by_id.insert(e.id.clone(), e);
        }
    }
    // (the catalog's own state wins over the library's)
    for (fid, r) in s.catalog.vignetting_maps() {
        by_id
            .entry(fid.clone())
            .or_insert_with(|| Entry { key: String::new(), id: fid.clone(), map: r.map.clone(), withdrawn: r.withdrawn })
            .withdrawn = r.withdrawn;
    }
    by_id.into_values().collect()
}

/// The maps of `pool` for photo `id`, each with how it fits, best first.
fn matched(s: &Session, pool: &[Entry], id: PhotoId) -> Vec<(Entry, MatchReport)> {
    let placement = s.photo_placement(id);
    let Some(photo) = s.catalog.photo(id).cloned() else { return Vec::new() };
    let mut items: Vec<(Entry, MatchReport)> = pool.iter().map(|e| (e.clone(), lib::match_lookup(&e.map, &photo, &placement))).collect();
    lib::sort_for(&mut items, &photo);
    items
}

/// Every map that could serve photo `id`, each with how it fits, best first.
pub(crate) fn candidates(s: &mut Session, id: PhotoId) -> Vec<(Entry, MatchReport)> {
    let p = pool(s);
    matched(s, &p, id)
}

/// The reference Apply picks for photo `id` on its own, or why there is none (`noReference`; `noReferenceAtAperture`:
/// there is one for the camera and lens, but not at the photo's aperture; `notRaw`): the reference in use that fits
/// (same camera model, same lens named on both, a zoom near the same focal length, the same aperture, the photo's
/// image area); the tighter image area first ([`lib::sort_for`]).
fn auto_pick(s: &Session, pool: &[Entry], id: PhotoId) -> std::result::Result<(Entry, MatchReport), &'static str> {
    let Some(photo) = s.catalog.photo(id) else { return Err("noReference") };
    if !photo.develops_raw() {
        return Err("notRaw");
    }
    let items = matched(s, pool, id);
    // (in use, and of the photo's lens: one either side doesn't name can't be checked, so it isn't picked by itself)
    let usable = |e: &Entry, r: &MatchReport| !e.withdrawn && !r.warnings.contains(&lib::Warning::LensUnknown);
    if let Some(hit) = items.iter().find(|(e, r)| usable(e, r) && r.fits) {
        return Ok(hit.clone());
    }
    // (the camera and lens fit, only the aperture doesn't: say so)
    let at_another_aperture =
        items.iter().any(|(e, r)| usable(e, r) && matches!(r.refused, Some(lib::Refusal::Aperture | lib::Refusal::ApertureUnknown)));
    Err(if at_another_aperture { "noReferenceAtAperture" } else { "noReference" })
}

fn list(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "vignettingCalibration.list";
    let id = photo_arg(s, p, C)?;
    let items: Vec<Value> = candidates(s, id)
        .into_iter()
        .map(|(e, r)| {
            let mut v = map_json(&e.id, &e.map);
            v["key"] = json!(e.key);
            v["inCatalog"] = json!(s.catalog.vignetting_map(&e.id).is_some());
            v["withdrawn"] = json!(e.withdrawn);
            v["match"] = report_json(&r);
            v
        })
        .collect();
    Ok(json!({"maps": items}))
}

/// The map `apply` names: `map` (an id in the catalog, or a portable map), else `key` (a library entry); `None` when it
/// names neither (Apply picks per photo).
fn map_param(s: &Session, p: &Value, c: &str) -> Result<Option<(VignettingMapId, EmbeddedVignettingMap)>> {
    if let Some(key) = str_param(p, "key") {
        let d = s.vignetting_calibration_dir().ok_or_else(|| bad(c, "no vignetting calibration library here"))?;
        let e = lib::load(&d, key).ok_or_else(|| bad(c, format!("no vignetting map `{key}` in the library")))?;
        return Ok(Some((e.id, e.map)));
    }
    match p.get("map") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let id = VignettingMapId::parse(text).ok_or_else(|| bad(c, "map must be a vignetting map id or a vignetting map"))?;
            if let Some(r) = s.catalog.vignetting_map(&id) {
                return Ok(Some((id, r.map.clone())));
            }
            let from_library = s.vignetting_calibration_dir().and_then(|d| lib::list(&d, None).into_iter().find(|e| e.id == id));
            from_library.map(|e| Some((e.id, e.map))).ok_or_else(|| bad(c, format!("no vignetting map {}", id.short())))
        }
        Some(v @ Value::Object(_)) => {
            let m = lcc_develop::vignetting_calibration::map_from_json(v).ok_or_else(|| bad(c, "not a valid vignetting map"))?;
            Ok(Some((VignettingMapId::of(&m), m)))
        }
        Some(_) => Err(bad(c, "map must be a vignetting map id or a vignetting map")),
    }
}

/// `vignettingCalibration.apply`: a vignetting calibration on the targets (the selection by default), as one undo step,
/// at amount 100. Without `map` or `key`, each photo gets the reference [`auto_pick`] finds for it; with one, that map
/// goes on every photo it fits. A photo without a fitting reference is skipped and listed; one whose image area is still
/// being read (or whose raw is offline) is checked by lens only (`unchecked`).
fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "vignettingCalibration.apply";
    let named = map_param(s, p, C)?;
    let ids = s.targets(p)?;
    if ids.is_empty() {
        return Err(bad(C, "no photos"));
    }
    let candidates = if named.is_none() { pool(s) } else { Vec::new() };
    let mut picked: Vec<(PhotoId, VignettingMapId, EmbeddedVignettingMap)> = Vec::new();
    let mut skipped = Vec::new();
    let mut unchecked = 0;
    for id in ids {
        let Some(photo) = s.catalog.photo(id).cloned() else { continue };
        let (map_id, map, report) = match &named {
            Some((map_id, map)) => (map_id.clone(), map.clone(), lib::match_lookup(map, &photo, &s.photo_placement(id))),
            None => match auto_pick(s, &candidates, id) {
                Ok((e, r)) => (e.id, e.map, r),
                Err(why) => {
                    skipped.push(json!({"id": id.0, "reason": why}));
                    continue;
                }
            },
        };
        match &report.refused {
            // a raw whose file can't be read now (offline): checked by lens only, like one still being read
            Some(lib::Refusal::NotRaw) if photo.develops_raw() => unchecked += 1,
            Some(why) => {
                skipped.push(json!({"id": id.0, "reason": why}));
                continue;
            }
            None => unchecked += usize::from(report.pending),
        }
        picked.push((id, map_id, map));
    }
    let maps: Vec<(VignettingMapId, EmbeddedVignettingMap)> = picked.iter().map(|(_, i, m)| (i.clone(), m.clone())).collect();
    let mut ops = crate::vignetting_calibration_io::add_ops(&s.catalog, &maps, &(s.clock)());
    let mut applied = Vec::new();
    for (id, map_id, _) in &picked {
        let Some(photo) = s.catalog.photo(*id) else { continue };
        let mut d = (*photo.develop).clone();
        if d.optics.vignetting_calibration.as_ref() == Some(map_id) {
            continue;
        }
        d.optics.vignetting_calibration = Some(map_id.clone());
        d.optics.vignetting_calibration_amount = lcc_develop::DEFAULT_VIGNETTING_CALIBRATION_PERCENT;
        ops.extend(s.develop_op(*id, d, "Apply Vignetting Calibration"));
        applied.push(json!({"id": id.0, "map": map_id.as_str()}));
    }
    if !applied.is_empty() {
        s.commit("Apply Vignetting Calibration", Op::Batch { ops })?;
    }
    Ok(json!({"applied": applied, "skipped": skipped, "unchecked": unchecked}))
}

/// `vignettingCalibration.remove`: no vignetting calibration on the targets (their amount stays), one undo step. The
/// map stays in the catalog.
fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = s.targets(p)?;
    let mut ops = Vec::new();
    for id in ids {
        let Some(photo) = s.catalog.photo(id) else { continue };
        if photo.develop.optics.vignetting_calibration.is_none() {
            continue;
        }
        let mut d = (*photo.develop).clone();
        d.optics.vignetting_calibration = None;
        ops.extend(s.develop_op(id, d, "Remove Vignetting Calibration"));
    }
    let n = ops.len();
    if n > 0 {
        s.commit("Remove Vignetting Calibration", Op::Batch { ops })?;
    }
    Ok(json!({"removed": n}))
}

/// `vignettingCalibration.photo`: what the Optics panel shows about photo `id`: its map and reference, and the
/// reference Apply would pick for it now (`auto`).
fn photo(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "vignettingCalibration.photo";
    let id = photo_arg(s, p, C)?;
    let Some(ph) = s.catalog.photo(id).cloned() else { return Err(bad(C, "no photo")) };
    let raw = ph.develops_raw();
    let candidates = pool(s);
    let auto = match auto_pick(s, &candidates, id) {
        Ok((e, r)) => {
            let mut v = map_json(&e.id, &e.map);
            v["match"] = report_json(&r);
            v
        }
        Err(why) => json!({"none": why, "aperture": ph.meta.aperture}),
    };
    let reference_of: Vec<Value> = references_of(s, id)
        .into_iter()
        .map(|(map_id, r)| {
            let mut v = map_json(&map_id, &r.map);
            v["withdrawn"] = json!(r.withdrawn);
            v
        })
        .collect();
    let Some(map_id) = ph.develop.optics.vignetting_calibration.clone() else {
        return Ok(
            json!({"map": null, "raw": raw, "amount": ph.develop.optics.vignetting_calibration_amount, "auto": auto, "referenceOf": reference_of}),
        );
    };
    let Some(rec) = s.catalog.vignetting_map(&map_id).cloned() else {
        return Ok(json!({
            "map": {"id": map_id.as_str()}, "missing": true, "raw": raw, "amount": ph.develop.optics.vignetting_calibration_amount,
            "auto": auto, "referenceOf": reference_of,
        }));
    };
    let r = lib::match_lookup(&rec.map, &ph, &s.photo_placement(id));
    let reference = lcc_catalog::reference_photo(&s.catalog, &map_id);
    let mut map = map_json(&map_id, &rec.map);
    map["withdrawn"] = json!(rec.withdrawn);
    Ok(json!({
        "map": map,
        "missing": false,
        "raw": raw,
        "amount": ph.develop.optics.vignetting_calibration_amount,
        "match": report_json(&r),
        "reference": {"photo": reference.map(|r| r.0), "inCatalog": reference.is_some()},
        "profileVignettingReplaced": ph.develop.optics.lens_profile,
        "sectionOff": !ph.develop.section_enabled("optics"),
        "auto": auto,
        "referenceOf": reference_of,
    }))
}

/// The catalog's maps photo `id` is the reference of ([`lcc_catalog::reference_photo`]), oldest first.
fn references_of(s: &Session, id: PhotoId) -> Vec<(VignettingMapId, std::sync::Arc<VignettingMapRecord>)> {
    if s.catalog.vignetting_reference_state(id).is_none() {
        return Vec::new();
    }
    let mut out: Vec<_> = s
        .catalog
        .vignetting_maps()
        .filter(|(map_id, _)| lcc_catalog::reference_photo(&s.catalog, map_id) == Some(id))
        .map(|(map_id, r)| (map_id.clone(), r.clone()))
        .collect();
    out.sort_by(|a, b| a.1.added.cmp(&b.1.added));
    out
}

/// `vignettingCalibration.withdraw` / `.restore`: withdraw maps as references (Apply doesn't pick them; the photos
/// that use them keep them) or put them back in use, as one undo step. `map` names one (in the catalog, or only in
/// the library: then its library file changes, not undoable); else every map photo `id` (the active photo) is the
/// reference of.
fn set_state(s: &mut Session, p: &Value, withdrawn: bool) -> Result<Value> {
    let c = if withdrawn { "vignettingCalibration.withdraw" } else { "vignettingCalibration.restore" };
    let label = if withdrawn { "Stop Using as Reference" } else { "Use as Reference Again" };
    let maps: Vec<VignettingMapId> = match p.get("map") {
        None | Some(Value::Null) => {
            let id = photo_arg(s, p, c)?;
            let maps: Vec<VignettingMapId> = references_of(s, id).into_iter().map(|(m, _)| m).collect();
            if maps.is_empty() {
                return Err(bad(c, "the photo is no vignetting reference"));
            }
            maps
        }
        Some(Value::String(text)) => {
            let id = VignettingMapId::parse(text).ok_or_else(|| bad(c, "map must be a vignetting map id"))?;
            if s.catalog.vignetting_map(&id).is_none() {
                // only in the library: its file
                let d = s.vignetting_calibration_dir().ok_or_else(|| bad(c, format!("no vignetting map {}", id.short())))?;
                let e = lib::list(&d, None).into_iter().find(|e| e.id == id).ok_or_else(|| bad(c, format!("no vignetting map {}", id.short())))?;
                lib::set_withdrawn(&d, &e.map, withdrawn).map_err(|e| bad(c, e))?;
                return Ok(json!({"changed": [id.as_str()], "library": true}));
            }
            vec![id]
        }
        Some(_) => return Err(bad(c, "map must be a vignetting map id")),
    };
    let ops: Vec<Op> = maps
        .iter()
        .filter(|m| s.catalog.vignetting_map(m).is_some_and(|r| r.withdrawn != withdrawn))
        .map(|m| Op::SetVignettingMapState { id: m.clone(), withdrawn })
        .collect();
    let changed: Vec<&str> = ops.iter().filter_map(|o| if let Op::SetVignettingMapState { id, .. } = o { Some(id.as_str()) } else { None }).collect();
    let changed = json!(changed);
    if !ops.is_empty() {
        s.commit(label, Op::Batch { ops })?;
    }
    Ok(json!({"changed": changed}))
}

fn withdraw(s: &mut Session, p: &Value) -> Result<Value> {
    set_state(s, p, true)
}

fn restore(s: &mut Session, p: &Value) -> Result<Value> {
    set_state(s, p, false)
}

/// `vignettingCalibration.references`: every reference, in this catalog and in the library, with its state.
fn references(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "vignettingCalibration.references";
    let want = match p.get("withdrawn") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(bad(C, "withdrawn must be true or false")),
    };
    let mut items: Vec<(String, Value)> = Vec::new();
    for e in pool(s) {
        if want.is_some_and(|w| w != e.withdrawn) {
            continue;
        }
        let rec = s.catalog.vignetting_map(&e.id);
        let photo = rec.and_then(|_| lcc_catalog::reference_photo(&s.catalog, &e.id));
        let added = rec.map(|r| r.added.clone()).unwrap_or_default();
        let mut v = map_json(&e.id, &e.map);
        v["key"] = json!(e.key);
        v["withdrawn"] = json!(e.withdrawn);
        v["inCatalog"] = json!(rec.is_some());
        v["photo"] = json!(photo.map(|p| p.0));
        v["added"] = json!(added);
        let when = e.map.source.captured.clone().unwrap_or_default();
        items.push((when, v));
    }
    // newest reference shot first
    items.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(json!({"references": items.into_iter().map(|(_, v)| v).collect::<Vec<_>>()}))
}

/// `vignettingCalibration.showReference`: make the reference photo of the active (or given) photo's map the active
/// photo.
fn show_reference(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "vignettingCalibration.showReference";
    let id = photo_arg(s, p, C)?;
    let map_id = s
        .catalog
        .photo(id)
        .and_then(|ph| ph.develop.optics.vignetting_calibration.clone())
        .ok_or_else(|| bad(C, "the photo has no vignetting calibration"))?;
    let reference = lcc_catalog::reference_photo(&s.catalog, &map_id).ok_or_else(|| bad(C, "the reference photo is no longer in this catalog"))?;
    s.execute("library.select", &json!({"ids": [reference.0], "active": reference.0}))?;
    Ok(json!({"id": reference.0}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "vignettingCalibration.create",
            "Create Vignetting Calibration",
            [],
            None,
            "{id?: the reference raw (default: the active photo), force?: bool (create a Borderline reference anyway), replace?: bool (replace the reference in use for the same camera model, lens, aperture and a zoom's focal length: it is withdrawn in the same undo step; photos that use it keep it)}: check that the raw looks like a flat-field reference (only an even surface, one colour, light falling off from one bright area in the middle), then measure the raw, shot of an even screen or through a diffuser, as a vignetting map (light fall-off only: one gain for all channels, no colour cast), keep it in the catalog and the library, and apply it to that photo at amount 100 (one undo step) → {map: {id, model, serial, lens, focal, aperture, fileName, captured}, key (library entry), falloff (EV at the darkest cell), uneven, centre: {dx, dy (% of the frame), rms}, exposure: {centre: [r, g, b], darkest, clipped (shares of white), verdict: ok | under | nearClip}, plausibility: {verdict: pass | passWithNote | borderline, reasons: [detail | colour | shape | noFalloff | dark | clipped], metrics: {detailMax, detailP99, detailRms, cellContrast, colour, peakOffset, falloff, risingRings, fitRms, level, clipped}}, vr: true | false | null (vibration reduction at capture, as the raw records it: better off for a reference), replaced: [map ids] (in use for the same setup and an image area the new one covers: the same, or inside it), keptForOtherAreas: [map ids] (in use for the same setup and image areas the new one doesn't cover: they stay, each for its own area)}; a raw that clearly isn't a flat field is refused with the reason, a Borderline one without force too, and one for a combination that has a reference in use without replace; measuring a reference the catalog holds again puts it back in use (after vignettingCalibration.withdraw)",
            has_active,
            create
        ),
        cmd!(
            query "vignettingCalibration.list",
            "List Vignetting Maps",
            [],
            None,
            "{id?: photo (default: the active one)} → {maps: [{id, key, model, serial, lens, focal, aperture, fileName, captured, inCatalog, match}]}: the library's and the catalog's maps for the photo, best match first (match as vignettingCalibration.photo's)",
            always,
            list
        ),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "vignettingCalibration.apply",
                "Apply Vignetting Calibration",
                [],
                None,
                "{map?: id | portable map, key?: library entry, ids?: photos (default: the selection)}: without map or key, each photo gets the reference in use for its combination (the same camera model and lens, a zoom within 5 % (at least 2 mm) of the focal length, the same aperture within a sixth of a stop; any body of the model); with one, that map on every photo it fits; at amount 100, one undo step → {applied: [{id, map}], skipped: [{id, reason: noReference | noReferenceAtAperture (one for the lens, not at the photo's aperture) | notRaw | anotherCamera | anotherLens | focalLength | aperture | apertureUnknown | imageArea}], unchecked (photos whose image area wasn't read yet: checked by lens only)}",
                has_selection,
                apply
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "vignettingCalibration.remove",
                "Remove Vignetting Calibration",
                [],
                None,
                "{ids?: photos (default: the selection)}: no vignetting calibration on them (the amount is kept; the map stays in the catalog), one undo step → {removed}",
                has_selection,
                remove
            )
        },
        cmd!(
            query "vignettingCalibration.photo",
            "Vignetting Calibration for Photo",
            [],
            None,
            "{id?} → {map: null | {id, model, serial, lens, focal, aperture, fileName, captured}, missing (the catalog lacks the map), raw, amount, match: {fits, refused?: notRaw | anotherCamera | anotherLens | focalLength | aperture (another aperture) | apertureUnknown (recorded on one side only) | imageArea, warnings: [{kind: lensUnknown | focalLength (diff mm, a zoom) | apertureUnknown (recorded on neither side) | uneven}], pending? (the photo's image area is still being read; ask again)}, reference: {photo, inCatalog}, profileVignettingReplaced, sectionOff, auto: the map Apply would pick ({id, …, match}) | {none: noReference | noReferenceAtAperture | notRaw, aperture: the photo's f-number}}",
            always,
            photo
        ),
        cmd!(
            query "vignettingCalibration.showReference",
            "Show Vignetting Calibration Reference",
            [],
            None,
            "{id?}: select the reference photo of the photo's vignetting calibration and make it active → {id}; refused when the reference is no longer in this catalog",
            has_active,
            show_reference
        ),
        cmd!(
            "vignettingCalibration.withdraw",
            "Stop Using as Reference",
            [],
            None,
            "{map?: vignetting map id, id?: photo (default: the active one)}: withdraw the map, or every map the photo is the reference of: Apply doesn't pick it any more, in any catalog; the photos that use it keep it, and the map stays (one undo step; a map only in the library has its library file changed) → {changed: [map ids], library?}",
            always,
            withdraw
        ),
        cmd!(
            "vignettingCalibration.restore",
            "Use as Reference Again",
            [],
            None,
            "{map?: vignetting map id, id?: photo (default: the active one)}: put a withdrawn map, or every map the photo is the reference of, back in use for Apply (one undo step) → {changed: [map ids], library?}",
            always,
            restore
        ),
        cmd!(
            query "vignettingCalibration.references",
            "List Vignetting References",
            [],
            None,
            "{withdrawn?: bool (only those withdrawn, or only those in use)} → {references: [{id, key (library entry, empty for a map only in this catalog), model, serial, lens, focal, aperture, fileName, captured, withdrawn, inCatalog, photo (the reference photo in this catalog, or null), added}]}: every vignetting reference in this catalog and in the library, the newest shot first",
            always,
            references
        ),
    ]
}
