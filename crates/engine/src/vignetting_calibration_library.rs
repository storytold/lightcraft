//! The per-body library of flat-field maps (design notes §10, *Storage*): the maps made in any catalog, to use in any
//! catalog. One JSON file per map in `<config>/flat-fields/<body>/<lens>_<focal>_<aperture>_<id8>.json`, each the map
//! as a catalog stores it (the same id). A catalog's own table is what renders; the library is where maps are found.
//! A damaged file is set aside as `.bad` with a warning, as the sensor maps' are (`sensor_maps`).
//!
//! [`match_photo`] says whether a map can serve a photo and what differs (design notes §10, *Matching*), and
//! [`sort_for`] orders a list best match first.

use std::path::{Path, PathBuf};

use lcc_catalog::Photo;
use lcc_develop::{EmbeddedVignettingMap, VignettingMapId};
use lcc_raw::gaintable::SourcePlacement;
use serde::{Deserialize, Serialize};

/// The library file format.
pub const VERSION: u32 = 1;
/// The largest library file read (a 256 × 256 map is about 180 KB).
const MAX_FILE: u64 = 1 << 20;
/// Focal lengths this close count as the same (mm).
const SAME_FOCAL: f64 = 0.5;
/// Apertures this close (stops) are the same: a sixth of a stop absorbs rounding (f/7.1 recorded as 7.0), while every
/// real step, down to third stops, stays apart.
pub const SAME_STOPS: f64 = 1.0 / 6.0;

/// The library's folder: `<PREFIX>_VIGNETTING_CALIBRATION` if set, else `flat-fields` in the settings folder. `None` without
/// one (the web build).
pub fn dir() -> Option<PathBuf> {
    lcc_brand::env_os("VIGNETTING_CALIBRATION")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::config::config_dir().map(|d| d.join("vignetting-calibration")))
}

/// One library file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryFile {
    pub version: u32,
    pub id: VignettingMapId,
    pub map: EmbeddedVignettingMap,
    /// Withdrawn as a reference: Apply doesn't pick it in any catalog (a catalog that holds the map follows its own
    /// state). Written whenever the catalog's state changes in a session ([`set_withdrawn`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub withdrawn: bool,
}

/// A map in the library: its key (`<body>/<file stem>`, what `vignettingCalibration.apply` takes) and the map.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub key: String,
    pub id: VignettingMapId,
    pub map: EmbeddedVignettingMap,
    /// Withdrawn as a reference ([`LibraryFile::withdrawn`]).
    pub withdrawn: bool,
}

/// A path part from free text: letters, digits, `-` and `.`, the rest `-`, at most 48 characters, never empty.
fn part(text: &str) -> String {
    let s: String = text.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '-' }).take(48).collect();
    let s = s.trim_matches(|c| c == '-' || c == '.').to_string();
    if s.is_empty() { "unknown".into() } else { s }
}

/// The body folder of a map: its camera's serial (as the sensor maps key bodies), else its model.
pub fn body_of(map: &EmbeddedVignettingMap) -> String {
    let s = &map.source;
    let key = crate::sensor_maps::body_key(s.serial.as_deref(), s.file_name.as_deref());
    part(key.as_deref().or(s.model.as_deref()).unwrap_or("unknown"))
}

/// A map's file name: lens, focal length, aperture and the id's first eight digits.
pub fn file_name(map: &EmbeddedVignettingMap, id: &VignettingMapId) -> String {
    let s = &map.source;
    let focal = s.focal_length.map_or("f-".into(), |f| format!("{}mm", f.round()));
    let aperture = s.aperture.map_or("N-".into(), |a| format!("f{a:.1}"));
    format!("{}_{}_{}_{}.json", part(s.lens.as_deref().unwrap_or("lens")), focal, aperture, id.short())
}

/// Save `map` in the library at `root` (atomic; a map already there is left as it is). Returns its entry.
pub fn save(root: &Path, map: &EmbeddedVignettingMap) -> Result<Entry, String> {
    if !map.is_valid() {
        return Err("not a valid vignetting map".into());
    }
    let id = VignettingMapId::of(map);
    let body = body_of(map);
    let name = file_name(map, &id);
    let folder = root.join(&body);
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(&name);
    if !path.is_file() {
        let file = LibraryFile { version: VERSION, id: id.clone(), map: map.clone(), withdrawn: false };
        let json = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
        lcc_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let stem = name.trim_end_matches(".json");
    Ok(Entry { key: format!("{body}/{stem}"), id, map: map.clone(), withdrawn: false })
}

/// Mark `map` in the library at `root` withdrawn or in use (atomic). A map the library doesn't hold is left out of
/// it (a map that came with a sidecar or from another catalog stays that catalog's own); a file already in that state
/// is left alone.
pub fn set_withdrawn(root: &Path, map: &EmbeddedVignettingMap, withdrawn: bool) -> Result<(), String> {
    let id = VignettingMapId::of(map);
    let path = root.join(body_of(map)).join(file_name(map, &id));
    if !path.is_file() {
        return Ok(());
    }
    let Some(mut file) = read(&path) else { return Err(format!("{}: not a usable map", path.display())) };
    if file.withdrawn == withdrawn {
        return Ok(());
    }
    file.withdrawn = withdrawn;
    let json = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    lcc_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))
}

/// Read one library file: `None` (and the file set aside as `.bad`) when it isn't a usable map.
fn read(path: &Path) -> Option<LibraryFile> {
    let why = match std::fs::metadata(path) {
        Ok(m) if m.len() > MAX_FILE => "too large".to_string(),
        Ok(_) => {
            match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<LibraryFile>(&b).map_err(|e| e.to_string())) {
                Ok(f) if f.version == VERSION && f.map.is_valid() && VignettingMapId::of(&f.map) == f.id => return Some(f),
                Ok(_) => "not a valid map of this format".to_string(),
                Err(e) => e,
            }
        }
        Err(_) => return None,
    };
    log::warn!("vignetting calibration library {}: {why}; set aside as .bad", path.display());
    let _ = std::fs::rename(path, path.with_extension("json.bad"));
    None
}

/// The library's maps, of one body (its folder name, [`body_of`]) or of all. Damaged files are set aside.
pub fn list(root: &Path, body: Option<&str>) -> Vec<Entry> {
    let mut out = Vec::new();
    let Ok(bodies) = std::fs::read_dir(root) else { return out };
    for b in bodies.flatten() {
        let name = b.file_name().to_string_lossy().to_string();
        if !b.path().is_dir() || body.is_some_and(|want| want != name) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(b.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_none_or(|e| e != "json") {
                continue;
            }
            if let Some(file) = read(&p) {
                let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                out.push(Entry { key: format!("{name}/{stem}"), id: file.id, map: file.map, withdrawn: file.withdrawn });
            }
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// The library map with this key (`<body>/<stem>`); `None` for a key that leaves the library (`..`, a separator or a
/// drive in a part) or names no usable map.
pub fn load(root: &Path, key: &str) -> Option<Entry> {
    let (body, stem) = key.split_once('/')?;
    let ok = |s: &str| !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', ':']) && s.len() <= 200;
    if !ok(body) || !ok(stem) {
        return None;
    }
    let file = read(&root.join(body).join(format!("{stem}.json")))?;
    Some(Entry { key: key.to_string(), id: file.id, map: file.map, withdrawn: file.withdrawn })
}

/// Why a map can't serve a photo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Refusal {
    /// Not a raw: there is no sensor to place the map on.
    NotRaw,
    /// Made with another camera model.
    AnotherCamera,
    /// The photo's image area isn't the map's, nor inside it on the same sensor.
    ImageArea,
    /// Made with another lens (both name theirs, and the names differ).
    AnotherLens,
    /// The same zoom, at a focal length too far from the reference's ([`same_zoom_focal`]).
    FocalLength,
    /// Shot at another aperture (more than [`SAME_STOPS`] apart): the fall-off changes with the aperture.
    Aperture,
    /// Only one of the photo and the reference records its aperture, so it can't be the same one.
    ApertureUnknown,
}

/// What differs between a map's reference shot and a photo (the map still applies).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Warning {
    /// The map or the photo names no lens, so the lens can't be checked.
    LensUnknown,
    /// The same zoom at a slightly other focal length (within [`same_zoom_focal`]), by `diff` mm (photo − reference).
    FocalLength { diff: f64 },
    /// Neither the map nor the photo records an aperture (an adapted or manual lens), so it can't be checked.
    ApertureUnknown,
    /// The reference wasn't lit evenly (its fall-off doesn't fit a lens's radial shape).
    Uneven,
}

/// How a map fits a photo.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchReport {
    pub fits: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<Refusal>,
    pub warnings: Vec<Warning>,
    /// The photo's placement on its sensor is still being read ([`crate::vignetting_calibration_placement`]): the image-area check
    /// hasn't run yet (ask again when it is known).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

fn same_text(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// How `map` fits `photo`, placed at `placement` (`None`: not a raw, or not known), per the design notes' table.
pub fn match_photo(map: &EmbeddedVignettingMap, photo: &Photo, placement: Option<&SourcePlacement>) -> MatchReport {
    match_with(map, photo, Some(placement))
}

/// [`match_photo`] for a placement [`crate::vignetting_calibration_placement::Placements`] may still be reading: while it is, the
/// report says so (`pending`) and everything but the image-area check runs.
pub fn match_lookup(map: &EmbeddedVignettingMap, photo: &Photo, placement: &crate::vignetting_calibration_placement::Lookup) -> MatchReport {
    match placement {
        crate::vignetting_calibration_placement::Lookup::Known(p) => match_photo(map, photo, p.as_ref()),
        crate::vignetting_calibration_placement::Lookup::Pending => match_with(map, photo, None),
    }
}

/// `placement`: `None` while it is being read.
fn match_with(map: &EmbeddedVignettingMap, photo: &Photo, placement: Option<Option<&SourcePlacement>>) -> MatchReport {
    let s = &map.source;
    let m = &photo.meta;
    let another_camera = s.model.as_deref().is_some_and(|model| !m.camera_model.trim().is_empty() && !same_text(model, &m.camera_model));
    // the lens: the same name, and a zoom near the same focal length; a lens either side doesn't name can't be checked
    let map_lens = s.lens.as_deref().map(str::trim).filter(|l| !l.is_empty());
    let photo_lens = Some(m.lens.trim()).filter(|l| !l.is_empty());
    let focal_diff = match (s.focal_length, m.focal_mm.map(f64::from)) {
        (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Some(b - a),
        _ => None,
    };
    let lens = match (map_lens, photo_lens) {
        (Some(a), Some(b)) if !same_text(a, b) => Err(Refusal::AnotherLens),
        (Some(a), Some(_)) if is_zoom(a) => match (focal_diff, m.focal_mm) {
            (Some(d), Some(f)) if d.abs() > same_zoom_focal(f64::from(f)) => Err(Refusal::FocalLength),
            _ => Ok(true),
        },
        (Some(_), Some(_)) => Ok(true),
        _ => Ok(false),
    };
    // the aperture: the same one only (within a sixth of a stop); recorded on neither side, it can't be checked
    let aperture = same_aperture(s.aperture, m.aperture.map(f64::from));
    let refused = match placement {
        Some(None) => Some(Refusal::NotRaw),
        _ if another_camera => Some(Refusal::AnotherCamera),
        _ if lens.is_err() => lens.clone().err(),
        _ if aperture.is_err() => aperture.clone().err(),
        None => None,
        Some(Some(p)) => {
            let placed = lcc_pipeline::vignetting_calibration::map_of(map).and_then(|f| f.sensor_at(p, 0.5, 0.5));
            placed.is_none().then_some(Refusal::ImageArea)
        }
    };
    let mut warnings = Vec::new();
    match lens {
        Ok(false) => warnings.push(Warning::LensUnknown),
        // (a zoom within reach: how far, quietly)
        Ok(true) if map_lens.is_some_and(is_zoom) => {
            if let Some(d) = focal_diff.filter(|d| d.abs() > SAME_FOCAL) {
                warnings.push(Warning::FocalLength { diff: d.round() });
            }
        }
        _ => {}
    }
    if aperture == Ok(false) {
        warnings.push(Warning::ApertureUnknown);
    }
    if uneven(map) {
        warnings.push(Warning::Uneven);
    }
    MatchReport { fits: refused.is_none(), refused, warnings, pending: placement.is_none() }
}

/// Whether a lens name is a zoom's: a focal-length range such as "24-70mm" or "100–400 mm".
pub fn is_zoom(lens: &str) -> bool {
    let b = lens.as_bytes();
    let digits_before = |i: usize| i > 0 && b.get(i - 1).is_some_and(u8::is_ascii_digit);
    let digit_after = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    lens.char_indices().any(|(i, c)| matches!(c, '-' | '–') && digits_before(i) && digit_after(i + c.len_utf8()))
}

/// How far (mm) a zoom photo's focal length may be from its map's: 5 % of the focal length, at least 2 mm.
pub fn same_zoom_focal(focal: f64) -> f64 {
    (0.05 * focal).max(2.0)
}

/// Whether apertures `a` and `b` (f-numbers) are the same: `Ok(true)` within [`SAME_STOPS`], `Ok(false)` when neither
/// is recorded (can't be checked), else the refusal.
pub fn same_aperture(a: Option<f64>, b: Option<f64>) -> Result<bool, Refusal> {
    let ok = |v: Option<f64>| v.filter(|v| *v > 0.0 && v.is_finite());
    match (ok(a), ok(b)) {
        (Some(a), Some(b)) if (2.0 * (b / a).log2()).abs() > SAME_STOPS + 1e-9 => Err(Refusal::Aperture),
        (Some(_), Some(_)) => Ok(true),
        (None, None) => Ok(false),
        _ => Err(Refusal::ApertureUnknown),
    }
}

/// Whether the map's fall-off doesn't fit a lens's radial shape (the centre fit's residual, relative to the fall-off).
pub fn uneven(map: &EmbeddedVignettingMap) -> bool {
    let Some(m) = lcc_pipeline::vignetting_calibration::map_of(map) else { return false };
    let grid = m.ev_grid();
    let corner = grid.ev.iter().copied().fold(0.0f64, f64::max);
    lcc_raw::vignetting_calibration::fit_centre(&grid).is_some_and(|fit| fit.uneven(corner))
}

/// Order maps for a photo, best first: those that fit, the same lens named, the nearest focal length, then the
/// smallest image area (a DX reference before an FX one for a DX shot: the tighter fit). Within a combination only one
/// reference is in use ([`same_combination`]); references made before that rule can still overlap, and the one shot
/// last comes first among them.
pub fn sort_for(items: &mut [(Entry, MatchReport)], photo: &Photo) {
    let m = &photo.meta;
    let key = |(e, r): &(Entry, MatchReport)| {
        let s = &e.map.source;
        let lens = s.lens.as_deref().is_some_and(|l| same_text(l, &m.lens));
        let focal = match (s.focal_length, m.focal_mm) {
            (Some(a), Some(b)) => (f64::from(b) - a).abs(),
            _ => f64::INFINITY,
        };
        let area = e.map.sensor[0].saturating_mul(e.map.sensor[1]);
        (!r.fits, !lens, focal, area, std::cmp::Reverse(s.captured.clone()))
    };
    items.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1.cmp(&kb.1)).then(ka.2.total_cmp(&kb.2)).then(ka.3.cmp(&kb.3)).then(ka.4.cmp(&kb.4))
    });
}

/// Whether two references serve the same photos, so only one of them may be in use: the same setup ([`same_setup`])
/// and the same image area.
pub fn same_combination(a: &EmbeddedVignettingMap, b: &EmbeddedVignettingMap) -> bool {
    same_setup(a, b) && coverage(a) == coverage(b)
}

/// The image area a reference covers on its camera's full sensor: the full sensor's size and the area (x, y, width,
/// height). A map that names no window covers its own sensor size, as a full sensor.
pub fn coverage(m: &EmbeddedVignettingMap) -> ([usize; 2], [usize; 4]) {
    match m.window {
        Some([fw, fh, x, y, w, h]) => ([fw, fh], [x, y, w, h]),
        None => (m.sensor, [0, 0, m.sensor[0], m.sensor[1]]),
    }
}

/// Whether reference `a` covers every part of the sensor reference `b` does (an FX reference covers a DX one on the
/// same sensor; equal areas cover each other).
pub fn covers(a: &EmbeddedVignettingMap, b: &EmbeddedVignettingMap) -> bool {
    let ((fa, [ax, ay, aw, ah]), (fb, [bx, by, bw, bh])) = (coverage(a), coverage(b));
    fa == fb && ax <= bx && ay <= by && ax.saturating_add(aw) >= bx.saturating_add(bw) && ay.saturating_add(ah) >= by.saturating_add(bh)
}

/// Whether two references are of the same setup: the same camera model, the same lens (or both unnamed), the same
/// aperture (within [`SAME_STOPS`], or both unrecorded) and for a zoom a focal length within [`same_zoom_focal`]. The
/// body's serial doesn't count. Within a setup, one reference is in use per image area; Create replaces those its new
/// reference covers ([`covers`]: an FX reference replaces a DX one) and keeps those it doesn't (a DX reference leaves
/// the FX one for full-frame shots).
pub fn same_setup(a: &EmbeddedVignettingMap, b: &EmbeddedVignettingMap) -> bool {
    let (x, y) = (&a.source, &b.source);
    let text = |p: Option<&str>, q: Option<&str>| match (p.map(str::trim).filter(|v| !v.is_empty()), q.map(str::trim).filter(|v| !v.is_empty())) {
        (Some(p), Some(q)) => same_text(p, q),
        (None, None) => true,
        _ => false,
    };
    let focal = match (x.lens.as_deref().filter(|l| is_zoom(l)), x.focal_length, y.focal_length) {
        (None, ..) => true,
        (Some(_), Some(f), Some(g)) => (f - g).abs() <= same_zoom_focal(f.max(g)),
        (Some(_), None, None) => true,
        _ => false,
    };
    text(x.model.as_deref(), y.model.as_deref())
        && text(x.lens.as_deref(), y.lens.as_deref())
        && same_aperture(x.aperture, y.aperture).is_ok()
        && focal
}

#[cfg(test)]
#[path = "tests_vignetting_calibration_library.rs"]
mod tests;
