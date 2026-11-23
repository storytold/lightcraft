//! Camera colour profiles of our own, pooled from many photos of one camera model (and look).
//!
//! A raw file without colour matrices (Sony ARW, Nikon NEF, Fujifilm RAF, Canon CRW) gets its look fitted to its own embedded camera
//! JPEG (`camera_preview`), but one photo shows too little of some colours: a lime shirt covering a
//! few dozen proxy pixels next to a hillside of foliage at the same hue. `lightcraft-cli calibrate`
//! pools the colour pairs of many photos per model — and, when the file records one, per picture
//! style / Film Simulation — and fits one matrix and hue/saturation/value table; photos of that
//! model then only fit their tone and chroma curves.
//!
//! Profiles are JSON files (`<model>.json`, or `<model>_<LOOK>.json` for a picture-style look) in
//! [`dir`]: `LIGHTCRAFT_CAMERA_PROFILES`, else `<config>/camera-profiles`; a local profile replaces
//! the one built in ([`BUNDLED`], from `assets/camera-profiles/`). They are read once per process for
//! the folder they are looked up in; a damaged or hostile file is ignored with a warning. They hold
//! aggregate colour statistics only, never image content.
use lightcraft_color::Mat3;
use lightcraft_raw::profile::HsvTable;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Format version of the profile files.
const VERSION: u32 = 1;
/// Largest profile file read (a 5 × 72 × 5 table is ~60 KB of JSON).
const MAX_FILE: u64 = 4 << 20;

/// Profiles built into LightCraft (`assets/camera-profiles/`, see `assets/ATTRIBUTION.md`):
/// `(id, JSON)`. `id` is the file stem (for errors); the profile's `model` / `look` fields select it.
pub const BUNDLED: &[(&str, &str)] = &[
    ("Canon EOS D60", include_str!("../../../assets/camera-profiles/Canon_EOS_D60.json")),
    ("ILCA-99M2", include_str!("../../../assets/camera-profiles/ILCA-99M2.json")),
    ("ILCE-7CR", include_str!("../../../assets/camera-profiles/ILCE-7CR.json")),
    ("ILCE-7M4", include_str!("../../../assets/camera-profiles/ILCE-7M4.json")),
    ("ILCE-7RM5", include_str!("../../../assets/camera-profiles/ILCE-7RM5.json")),
    ("NIKON D5300", include_str!("../../../assets/camera-profiles/NIKON_D5300.json")),
    ("NIKON Z 6_2", include_str!("../../../assets/camera-profiles/NIKON_Z_6_2.json")),
    ("NIKON Z 8", include_str!("../../../assets/camera-profiles/NIKON_Z_8.json")),
    // Fujifilm look variants: id is the file stem; `model` / `look` in the JSON select them.
    ("X-E5_PROVIA", include_str!("../../../assets/camera-profiles/X-E5_PROVIA.json")),
    ("X-E5_NOSTALGIC", include_str!("../../../assets/camera-profiles/X-E5_NOSTALGIC.json")),
    ("X-E5_VELVIA", include_str!("../../../assets/camera-profiles/X-E5_VELVIA.json")),
    ("X-H2S", include_str!("../../../assets/camera-profiles/X-H2S.json")),
    ("X-T4", include_str!("../../../assets/camera-profiles/X-T4.json")),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraProfile {
    pub version: u32,
    /// The camera model as the files report it (Exif `Model`), e.g. `ILCE-7M4` or `X-E5`.
    pub model: String,
    /// Picture style / Film Simulation this profile was fitted for (e.g. `PROVIA`, `VELVIA`).
    /// Absent or empty = no look key (Sony/Nikon bodies, or older Fujifilm PROVIA files named `<model>.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<String>,
    /// Photos and colour pairs the profile was fitted on.
    pub files: usize,
    pub samples: usize,
    /// White-balanced camera RGB (with the baseline exposure) → linear Rec.2020, rows.
    matrix: [[f64; 3]; 3],
    /// Hue/saturation/value correction after `matrix` (linear ProPhoto RGB).
    pub hue_sat: Option<HsvTable>,
}

impl CameraProfile {
    pub fn new(model: &str, look: Option<&str>, files: usize, samples: usize, matrix: Mat3, hue_sat: Option<HsvTable>) -> CameraProfile {
        CameraProfile { version: VERSION, model: model.to_owned(), look: normalize_look(look), files, samples, matrix: matrix.0, hue_sat }
    }

    pub fn matrix(&self) -> Mat3 {
        Mat3(self.matrix)
    }

    /// Look key used for file names and lookup (`None` = default).
    pub fn look_key(&self) -> Option<&str> {
        self.look.as_deref()
    }

    /// Whether the data is usable (bounded matrix, table shape matching its data).
    fn valid(&self) -> bool {
        let matrix = self.matrix.iter().flatten().all(|v| v.is_finite() && v.abs() < 8.0) && Mat3(self.matrix).inverse().is_some();
        let table = self.hue_sat.as_ref().is_none_or(|t| {
            let dims = (1..=4096).contains(&t.hue_divisions) && (2..=4096).contains(&t.sat_divisions) && (1..=4096).contains(&t.val_divisions);
            let len = t.hue_divisions.checked_mul(t.sat_divisions).and_then(|n| n.checked_mul(t.val_divisions));
            dims && len == Some(t.data.len()) && t.data.iter().flatten().all(|v| v.is_finite()) && t.data.iter().all(|e| e[1] >= 0.0 && e[2] >= 0.0)
        });
        let look_ok = self.look.as_ref().is_none_or(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        self.version == VERSION && !self.model.is_empty() && matrix && table && look_ok
    }
}

/// LightCraft's configuration folder (settings, GPU marker, camera profiles); one helper for the app.
pub use crate::config::config_dir;

/// Where camera profiles are read from and written to.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("LIGHTCRAFT_CAMERA_PROFILES").map(PathBuf::from).or_else(|| config_dir().map(|d| d.join("camera-profiles")))
}

fn sanitize(s: &str) -> String {
    s.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// File name of a profile: `<model>.json`, or `<model>_<LOOK>.json` when a look is set.
pub fn file_name(model: &str, look: Option<&str>) -> String {
    let base = sanitize(model);
    match normalize_look(look) {
        Some(l) => format!("{base}_{}.json", sanitize(&l)),
        None => format!("{base}.json"),
    }
}

/// Empty → `None`; `STANDARD` / `F0` → `PROVIA`; other looks kept as trimmed ASCII uppercase.
pub fn normalize_look(look: Option<&str>) -> Option<String> {
    let l = look.map(str::trim).filter(|s| !s.is_empty())?;
    let upper = l.to_ascii_uppercase();
    Some(match upper.as_str() {
        "STANDARD" | "F0" => "PROVIA".to_owned(),
        _ => upper,
    })
}

/// Cache / pool key for a model + look.
fn cache_id(model: &str, look: Option<&str>) -> String {
    match normalize_look(look) {
        Some(l) => format!("{model}\0{l}"),
        None => model.to_owned(),
    }
}

fn looks_equal(a: Option<&str>, b: Option<&str>) -> bool {
    normalize_look(a) == normalize_look(b)
}

/// Read and validate a profile file.
pub fn load(path: &Path) -> Result<CameraProfile, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if size > MAX_FILE {
        return Err(format!("{}: {size} bytes is too large for a camera profile", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&bytes, &path.display().to_string())
}

/// Parse and validate a profile's JSON (`origin` names it in errors).
fn parse(json: &[u8], origin: &str) -> Result<CameraProfile, String> {
    let profile: CameraProfile = serde_json::from_slice(json).map_err(|e| format!("{origin}: {e}"))?;
    if !profile.valid() {
        return Err(format!("{origin}: not a usable camera profile (version {}, model {:?})", profile.version, profile.model));
    }
    Ok(profile)
}

/// Model spelling with case and spacing folded away, for matching EXIF variants.
fn normalized_model(model: &str) -> String {
    model.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

/// EXIF spellings that differ by more than case and spacing, as `(spelling, bundled model)`.
const ALIASES: &[(&str, &str)] = &[
    ("NIKON Z8", "NIKON Z 8"),
    ("NIKON Z 6-2", "NIKON Z 6_2"),
    ("NIKON Z 6 2", "NIKON Z 6_2"),
    ("Nikon Z8", "NIKON Z 8"),
    ("Nikon Z 6-2", "NIKON Z 6_2"),
    ("Nikon Z 6 2", "NIKON Z 6_2"),
];

/// Normalize a model name for local profile lookup (case, whitespace, and separator folding).
fn normalize_for_lookup(model: &str) -> String {
    normalized_model(model).replace(['-', ' '], "_")
}

/// Normalize a model name for local profile matching: [`ALIASES`] applied as for bundled
/// profiles, then case, whitespace and separators folded to `_`, so a profile saved under one
/// spelling ("NIKON Z8", `NIKON_Z8.json`) is found by another ("NIKON Z 8").
fn normalize_for_local_match(model: &str) -> String {
    let folded = normalize_for_lookup(model);
    ALIASES.iter().find(|(spelling, _)| normalize_for_lookup(spelling) == folded).map_or(folded, |(_, canonical)| normalize_for_lookup(canonical))
}

/// Resolve an EXIF model spelling to the canonical model spelling using
/// [`ALIASES`], so spellings that differ by more than case and spacing
/// (for example "NIKON Z8" versus "NIKON Z 8") match the same profile.
fn canonical_model(model: &str) -> &str {
    let needle = normalized_model(model);
    ALIASES.iter().find(|(spelling, _)| normalized_model(spelling) == needle).map_or(model, |(_, canonical)| *canonical)
}

/// Stem used when ranking local files for `model` + `look`.
fn stem_needle(model: &str, look: Option<&str>) -> String {
    let name = file_name(model, look);
    let stem = name.trim_end_matches(".json");
    normalize_for_local_match(stem)
}

/// The built-in profile for `model` + `look`, if any.
fn bundled(model: &str, look: Option<&str>) -> Option<CameraProfile> {
    let needle = normalize_for_local_match(model);
    let look = normalize_look(look);
    for (id, json) in BUNDLED {
        let Ok(p) = parse(json.as_bytes(), &format!("built-in profile {id}")) else {
            eprintln!("lightcraft: ignoring camera profile built-in {id}");
            continue;
        };
        if normalize_for_local_match(&p.model) == needle && looks_equal(p.look_key(), look.as_deref()) {
            return Some(p);
        }
    }
    None
}

/// Write `profile` to `dir` (created if needed); returns the path.
pub fn save(profile: &CameraProfile, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(file_name(&profile.model, profile.look_key()));
    let json = serde_json::to_vec_pretty(profile).map_err(|e| e.to_string())?;
    // a fresh temp file renamed over the profile: never a fixed `.tmp` name, which a planted link
    // would have written through
    lightcraft_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Profiles already read, per folder they were read from and model+look: a lookup pointed at another
/// folder (a test's temporary directory) must not return the profile read from this one.
type Cache = HashMap<(Option<PathBuf>, String), Option<Arc<CameraProfile>>>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The default profile for camera `model` (no picture-style look).
pub fn get(model: &str) -> Option<Arc<CameraProfile>> {
    get_look(model, None)
}

/// The profile for `model` and optional picture-style `look`.
///
/// Velvia / Nostalgic / Astia never fall back to another Film Simulation. `PROVIA` also tries the
/// legacy `<model>.json` (no `look` field) so older Fujifilm PROVIA bundles keep working.
pub fn get_look(model: &str, look: Option<&str>) -> Option<Arc<CameraProfile>> {
    get_from(model, look, dir().as_deref())
}

/// Profile for this raw: model + Fujifilm FilmMode (or other picture style) when present.
pub fn for_file(model: Option<&str>, bytes: &[u8]) -> Option<Arc<CameraProfile>> {
    let model = model.map(str::trim).filter(|m| !m.is_empty())?;
    get_look(model, picture_look(bytes).as_deref())
}

/// Picture-style / Film Simulation key from the file (Fujifilm FilmMode today), if any.
pub fn picture_look(bytes: &[u8]) -> Option<String> {
    lightcraft_meta::picture_look(bytes)
}

/// The profile for `model` + `look` read from `directory`, else built in; read once per process for
/// that folder.
///
/// `directory` is the profile folder to search for a local profile; [`dir`] resolves it from
/// [`LIGHTCRAFT_CAMERA_PROFILES`] or the config folder. Separating it lets tests point the lookup
/// at a temporary directory without changing process environment variables.
fn get_from(model: &str, look: Option<&str>, directory: Option<&Path>) -> Option<Arc<CameraProfile>> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    // EXIF spellings that differ by more than case and spacing (for example
    // "NIKON Z8") resolve to the canonical model ("NIKON Z 8") before the
    // local lookup, and the profile's own model is compared through the same
    // aliases, so a local profile saved under either spelling is found
    // instead of falling back to the bundled profile.
    let model = canonical_model(model);
    let look_norm = normalize_look(look);
    let look = look_norm.as_deref();
    let key = (directory.map(Path::to_path_buf), cache_id(model, look));
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let profile = load_local(model, look, directory).or_else(|| bundled(model, look)).map(Arc::new);
    // Older Fujifilm PROVIA profiles are still `<model>.json` with no look field.
    let profile = match (&profile, look) {
        (None, Some("PROVIA")) => load_local(model, None, directory).or_else(|| bundled(model, None)).map(Arc::new),
        _ => profile,
    };
    cache.insert(key, profile.clone());
    profile
}

/// Scan `directory` for a local profile matching `model` + `look`.
///
/// Candidates are regular `.json` files ranked so a normalized stem match beats a model-name match;
/// within a rank the sorted path order decides. Only profiles whose `look` matches are accepted, so
/// `X-E5_VELVIA.json` never answers a default-model lookup.
fn load_local(model: &str, look: Option<&str>, directory: Option<&Path>) -> Option<CameraProfile> {
    let model_needle = normalize_for_local_match(model);
    let stem = stem_needle(model, look);
    directory.and_then(|d| {
        let mut entries: Vec<_> = std::fs::read_dir(d).ok()?.flatten().collect();
        entries.sort_by_key(|e| e.path());
        let mut ranked: Vec<(u8, PathBuf)> = Vec::new();
        for e in &entries {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") || !path.is_file() {
                continue;
            }
            let rank = if path.file_stem().is_some_and(|s| normalize_for_local_match(&s.to_string_lossy()) == stem) { 1 } else { 2 };
            ranked.push((rank, path));
        }
        ranked.sort();
        for (_, path) in ranked {
            if let Some(p) = load(&path).ok().filter(|p| normalize_for_local_match(&p.model) == model_needle && looks_equal(p.look_key(), look)) {
                return Some(p);
            }
        }
        None
    })
}

/// Changes when the profiles folder's contents change (per process): part of the render cache
/// keys, so thumbnails rendered before a profile existed are not reused after.
pub fn cache_key() -> u64 {
    static KEY: OnceLock<u64> = OnceLock::new();
    *KEY.get_or_init(|| cache_key_for(dir().as_deref()))
}

fn cache_key_for(directory: Option<&Path>) -> u64 {
    let entries = directory.and_then(|d| std::fs::read_dir(d).ok());
    let mut files: Vec<(String, u64, std::time::Duration)> = entries
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
            Some((e.file_name().to_string_lossy().into_owned(), meta.len(), modified))
        })
        .collect();
    files.sort();
    let mut h = lightcraft_preview::Hasher128::new();
    for (name, len, modified) in &files {
        h.str(name).u64(*len).u64(modified.as_secs()).u64(u64::from(modified.subsec_nanos()));
    }
    for (id, json) in BUNDLED {
        h.str(id).str(json);
    }
    h.finish().0 as u64
}

/// Per camera model (+ look): the photos read and their pooled colour pairs.
#[derive(Default)]
pub struct Pool {
    /// Key = [`cache_id`](model, look).
    models: HashMap<String, PoolEntry>,
}

struct PoolEntry {
    model: String,
    look: Option<String>,
    files: usize,
    pairs: Vec<([f64; 3], [f64; 3])>,
}

/// Most colour pairs kept per photo, so a few busy photos can't dominate a profile.
const PAIRS_PER_FILE: usize = 4000;

impl Pool {
    /// Add one raw file's colour pairs. `Ok(None)` when the file can't contribute (not an ARW,
    /// NEF or RAF without colour matrices, no usable camera JPEG, too little colour).
    /// On success returns a label like `X-E5` or `X-E5/VELVIA`.
    pub fn add(&mut self, bytes: &[u8]) -> Result<Option<String>, String> {
        let mut raw = lightcraft_raw::decode(bytes).map_err(|e| e.to_string())?;
        // colour only: geometric lens corrections would stop the sensor proxy from binning (the pairs still follow
        // the warp the camera applied to its JPEG)
        let lens = crate::camera_preview::lens_geometry(&raw.opcodes.list3);
        raw.opcodes.list3.retain(|op| !op.is_lens_correction());
        let Some(model) = raw.metadata.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) else { return Ok(None) };
        let look = picture_look(bytes);
        let Some(pairs) = crate::camera_preview::profile_pairs(&raw, bytes, lens.as_ref()) else { return Ok(None) };
        let step = pairs.len().div_ceil(PAIRS_PER_FILE).max(1);
        let id = cache_id(model, look.as_deref());
        let label = match look.as_deref() {
            Some(l) => format!("{model}/{l}"),
            None => model.to_owned(),
        };
        let entry = self.models.entry(id).or_insert_with(|| PoolEntry { model: model.to_owned(), look: look.clone(), files: 0, pairs: Vec::new() });
        entry.files += 1;
        entry.pairs.extend(pairs.into_iter().step_by(step));
        Ok(Some(label))
    }

    /// Fit a profile per model/look with at least `min_files` photos.
    pub fn fit(&self, min_files: usize) -> Vec<Result<CameraProfile, String>> {
        let mut entries: Vec<_> = self.models.values().collect();
        entries.sort_by(|a, b| (&a.model, &a.look).cmp(&(&b.model, &b.look)));
        entries
            .into_iter()
            .filter(|e| e.files >= min_files)
            .map(|e| {
                let label = match e.look.as_deref() {
                    Some(l) => format!("{}/{}", e.model, l),
                    None => e.model.clone(),
                };
                let (matrix, hue_sat) = crate::camera_preview::fit_profile(&e.pairs).ok_or_else(|| format!("{label}: no usable colour fit"))?;
                Ok(CameraProfile::new(&e.model, e.look.as_deref(), e.files, e.pairs.len(), matrix, hue_sat))
            })
            .collect()
    }

    /// Add another pool's photos and pairs (pools filled on separate threads).
    pub fn merge(&mut self, other: Pool) {
        for (id, o) in other.models {
            let entry =
                self.models.entry(id).or_insert_with(|| PoolEntry { model: o.model.clone(), look: o.look.clone(), files: 0, pairs: Vec::new() });
            entry.files += o.files;
            entry.pairs.extend(o.pairs);
        }
    }

    /// Photos read per model/look label.
    pub fn files(&self) -> Vec<(String, usize)> {
        let mut v: Vec<_> = self
            .models
            .values()
            .map(|e| {
                let label = match e.look.as_deref() {
                    Some(l) => format!("{}/{}", e.model, l),
                    None => e.model.clone(),
                };
                (label, e.files)
            })
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> CameraProfile {
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[5.0, 1.2, 1.0]; 8], srgb_value: false };
        CameraProfile::new("ILCE-7M4", None, 12, 3456, Mat3([[1.6, -0.5, -0.1], [-0.2, 1.3, -0.1], [-0.1, -0.2, 1.3]]), Some(table))
    }

    #[test]
    fn profiles_round_trip_and_reject_bad_files() {
        let dir = std::env::temp_dir().join(format!("lc-camera-profiles-{}", std::process::id()));
        let path = save(&profile(), &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "ILCE-7M4.json");
        assert_eq!(load(&path).unwrap(), profile());
        let bad = |p: CameraProfile| {
            let path = dir.join("bad.json");
            std::fs::write(&path, serde_json::to_vec(&p).unwrap()).unwrap();
            load(&path).is_err()
        };
        let mut p = profile();
        p.matrix[0][0] = f64::NAN;
        assert!(bad(p), "non-finite matrix");
        let mut p = profile();
        p.matrix = [[0.0; 3]; 3];
        assert!(bad(p), "singular matrix");
        let mut p = profile();
        if let Some(t) = p.hue_sat.as_mut() {
            t.data.truncate(3);
        }
        assert!(bad(p), "table data doesn't match its shape");
        let mut p = profile();
        p.version = 99;
        assert!(bad(p), "unknown version");
        std::fs::write(dir.join("junk.json"), b"{not json").unwrap();
        assert!(load(&dir.join("junk.json")).is_err());
        assert!(load(&dir.join("missing.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn look_profiles_save_under_model_look_names() {
        let dir = std::env::temp_dir().join(format!("lc-camera-profiles-look-{}", std::process::id()));
        let mut p = profile();
        p.model = "X-E5".into();
        p.look = Some("VELVIA".into());
        let path = save(&p, &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "X-E5_VELVIA.json");
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.look_key(), Some("VELVIA"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_profiles_are_valid_and_named_after_their_model() {
        assert!(!BUNDLED.is_empty());
        let mut seen = std::collections::HashSet::new();
        for (id, json) in BUNDLED {
            let p = parse(json.as_bytes(), id).unwrap();
            assert!(p.files >= 5 && p.hue_sat.is_some(), "{id}: {} photos", p.files);
            let key = (p.model.clone(), p.look_key().map(str::to_owned));
            assert!(seen.insert(key), "{id}: duplicate model/look");
            assert_eq!(bundled(&p.model, p.look_key()).as_ref(), Some(&p));
        }
        assert!(bundled("No Such Camera", None).is_none());
        assert!(bundled("Canon EOS D60", None).is_some_and(|p| p.files > 1000));
        assert_eq!(file_name("Canon EOS D60", None), "Canon_EOS_D60.json");
        assert_eq!(get_look("X-E5", Some("PROVIA")).as_ref().map(|p| p.look_key()), Some(Some("PROVIA")));
        assert_eq!(get_look("X-E5", Some("VELVIA")).as_ref().map(|p| p.look_key()), Some(Some("VELVIA")));
        assert_eq!(get_look("X-E5", Some("NOSTALGIC")).as_ref().map(|p| p.look_key()), Some(Some("NOSTALGIC")));
        // Velvia must not fall back to PROVIA.
        assert!(get_look("X-T4", Some("VELVIA")).is_none());
        // Legacy Fujifilm PROVIA files without a look field still answer PROVIA.
        assert!(get_look("X-T4", Some("PROVIA")).is_some());
        assert!(get_look("X-T4", None).is_some());
    }

    #[test]
    fn bundled_profile_matches_nikon_model_spelling_variants() {
        for (variant, canonical) in [
            ("Nikon D5300", "NIKON D5300"),
            ("Nikon  D5300", "NIKON D5300"),
            (" Nikon D5300 ", "NIKON D5300"),
            ("Nikon Z 6_2", "NIKON Z 6_2"),
            ("Nikon  Z  6_2", "NIKON Z 6_2"),
            ("Nikon Z 8", "NIKON Z 8"),
            ("Nikon  Z  8", "NIKON Z 8"),
            ("Nikon Z8", "NIKON Z 8"),
            ("NIKON Z8", "NIKON Z 8"),
            ("Nikon Z 6-2", "NIKON Z 6_2"),
            ("Nikon Z 6 2", "NIKON Z 6_2"),
        ] {
            assert!(bundled(canonical, None).is_some(), "the bundled {canonical:?} profile must load");
            assert_eq!(bundled(variant, None), bundled(canonical, None), "NEF spelling {variant:?} must find the bundled {canonical:?} profile");
        }
    }

    #[test]
    fn bundled_profile_matches_nikon_model_spelling_variants_review_regression_1() {
        // Regression test for:
        // 1. Model normalization only folds case/whitespace; EXIF spellings with different
        //    separators (Z8, Z 6-2, Z 6 2) need explicit aliases
        // 2. Local profile lookup uses exact string match instead of normalized matching
        // 3. Staleness test uses dynamic version derivation making it pass incorrectly if
        //    LOOK_VERSION reverts (tested in smart/tests.rs)

        // After fix: EXIF spellings with different separators should match bundled profile via ALIASES
        assert!(bundled("Nikon Z 6-2", None).is_some(), "Nikon Z 6-2 should match bundled NIKON Z 6_2 profile");
        assert!(bundled("Nikon Z 6 2", None).is_some(), "Nikon Z 6 2 should match bundled NIKON Z 6_2 profile");
        // Z8 already works via ALIASES
        assert_eq!(bundled("Nikon Z8", None), bundled("NIKON Z 8", None), "Nikon Z8 should continue to match via ALIASES");
    }

    #[test]
    fn same_second_profile_replacement_changes_render_cache_key() {
        let dir = std::env::temp_dir().join(format!("lc-camera-profile-cache-{}", std::process::id()));
        let mut first = profile();
        first.model = "ILCE-7CR".into();
        let path = save(&first, &dir).unwrap();
        let set_modified = |millis| {
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000) + std::time::Duration::from_millis(millis);
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
            file.metadata().unwrap()
        };
        let before = set_modified(100);
        assert_eq!(load(&path).unwrap(), first);
        let before_key = cache_key_for(Some(&dir));

        let mut second = first;
        second.matrix[0][0] = 1.7;
        save(&second, &dir).unwrap();
        let after = set_modified(350);
        assert_eq!(load(&path).unwrap(), second);
        assert_eq!(before.len(), after.len());
        let before_time = before.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap();
        let after_time = after.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap();
        assert_eq!(before_time.as_secs(), after_time.as_secs());
        assert_ne!(before_time.subsec_nanos(), after_time.subsec_nanos());
        let after_key = cache_key_for(Some(&dir));
        let _ = std::fs::remove_dir_all(dir);

        assert_ne!(before_key, after_key, "a changed profile must not reuse the previous render cache");
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("ILCE-7M4", None), "ILCE-7M4.json");
        assert_eq!(file_name(" DSC-RX100M3 ", None), "DSC-RX100M3.json");
        assert_eq!(file_name("../../etc/passwd", None), "______etc_passwd.json");
        assert_eq!(file_name("Ω 1/2", None), "__1_2.json");
        assert_eq!(file_name("X-E5", Some("VELVIA")), "X-E5_VELVIA.json");
        assert_eq!(file_name("X-E5", Some("PROVIA")), "X-E5_PROVIA.json");
    }

    #[test]
    fn normalize_look_keeps_provia_explicit() {
        assert_eq!(normalize_look(Some("PROVIA")).as_deref(), Some("PROVIA"));
        assert_eq!(normalize_look(Some("provia")).as_deref(), Some("PROVIA"));
        assert_eq!(normalize_look(Some("STANDARD")).as_deref(), Some("PROVIA"));
        assert_eq!(normalize_look(Some("VELVIA")).as_deref(), Some("VELVIA"));
        assert_eq!(normalize_look(Some("velvia")).as_deref(), Some("VELVIA"));
        assert_eq!(normalize_look(None), None);
    }

    #[test]
    fn local_profile_lookup_uses_normalized_matching() {
        // Local profiles saved under one spelling should be found by another
        // spelling that normalizes to the same key.
        let dir = std::env::temp_dir().join(format!("lc-local-profile-lookup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Save a profile under the canonical spelling
        let profile = CameraProfile::new("NIKON Z 6_2", None, 10, 100, Mat3::IDENTITY, None);
        let path = save(&profile, &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "NIKON_Z_6_2.json");

        // Point the lookup at the temporary directory so it finds the local profile
        // instead of the bundled one.
        let found = get_from("Nikon Z 6-2", None, Some(&dir));
        assert!(found.is_some(), "Nikon Z 6-2 should find local NIKON Z 6_2 profile");
        assert_eq!(found.as_ref().unwrap().model, "NIKON Z 6_2");
        assert_eq!(found.as_ref().unwrap().files, 10, "the local NIKON Z 6_2 profile, not the bundled one, must be used");

        let found = get_from("Nikon Z 6 2", None, Some(&dir));
        assert!(found.is_some(), "Nikon Z 6 2 should find local NIKON Z 6_2 profile");
        assert_eq!(found.as_ref().unwrap().model, "NIKON Z 6_2");
        assert_eq!(found.as_ref().unwrap().files, 10, "the local NIKON Z 6_2 profile, not the bundled one, must be used");

        let found = get_from("NIKON Z 6-2", None, Some(&dir));
        assert!(found.is_some(), "NIKON Z 6-2 should find local NIKON Z 6_2 profile");
        assert_eq!(found.as_ref().unwrap().model, "NIKON Z 6_2");
        assert_eq!(found.as_ref().unwrap().files, 10, "the local NIKON Z 6_2 profile, not the bundled one, must be used");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_profile_lookup_uses_normalized_matching_review_regression_1() {
        // Regression: a local NIKON Z 8 profile must be found by the EXIF
        // spelling "NIKON Z8" (alias "NIKON Z8" -> "NIKON Z 8") instead of
        // falling back to the bundled NIKON Z 8 profile.
        // The lookup is pointed at a temporary directory so the local profile
        // is visible instead of the bundled one.
        // The assertion fails until local lookup applies the same alias table
        // as bundled().
        let dir = std::env::temp_dir().join(format!("lc-local-z8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let local = CameraProfile::new("NIKON Z 8", None, 1, 100, Mat3::IDENTITY, None);
        let path = save(&local, &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "NIKON_Z_8.json");

        let found = get_from("NIKON Z8", None, Some(&dir));
        assert!(found.is_some(), "NIKON Z8 should find a NIKON Z 8 profile");
        assert_eq!(found.unwrap().files, 1, "the local NIKON Z 8 profile, not the bundled one, must be used");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_profile_saved_under_the_exif_spelling_is_found() {
        // A profile calibrated from files that report "NIKON Z8" is saved as `NIKON_Z8.json`;
        // asking for the canonical "NIKON Z 8" must find it instead of the bundled profile.
        let dir = std::env::temp_dir().join(format!("lc-local-z8-exif-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let local = CameraProfile::new("NIKON Z8", None, 2, 100, Mat3::IDENTITY, None);
        let path = save(&local, &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "NIKON_Z8.json");

        let found = get_from("NIKON Z 8", None, Some(&dir)).expect("a NIKON Z 8 profile");
        assert_eq!(found.files, 2, "the local NIKON Z8 profile, not the bundled NIKON Z 8 one, must be used");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pool_skips_files_that_cannot_contribute() {
        let mut pool = Pool::default();
        assert!(pool.add(b"not a raw file").is_err());
        assert!(pool.fit(1).is_empty());
        assert!(pool.files().is_empty());
    }

    #[test]
    fn local_look_profile_is_not_returned_for_default_lookup() {
        let dir = std::env::temp_dir().join(format!("lc-local-look-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let velvia = CameraProfile::new("X-E5", Some("VELVIA"), 6, 100, Mat3::IDENTITY, None);
        save(&velvia, &dir).unwrap();
        assert!(get_from("X-E5", None, Some(&dir)).is_none(), "a Velvia-only folder must not answer a default lookup");
        assert_eq!(get_from("X-E5", Some("VELVIA"), Some(&dir)).as_ref().map(|p| p.files), Some(6));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_profile_lookup_uses_normalized_matching_review_regression_2() {
        let dir = std::env::temp_dir().join(format!("lc-local-profile-review-2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A non-regular file whose name normalizes to the lookup key. The scan must
        // skip it instead of calling `load` on it (a FIFO would block; a directory is
        // not a profile). The name contains a space so it cannot collide with the real
        // profile files on a case-insensitive file system.
        std::fs::create_dir(dir.join("NIKON D5300.json")).unwrap();

        // Two profiles whose names normalize to the same lookup key. The file names
        // differ by separator (not only case) so they stay distinct on case-insensitive
        // file systems; sorted, `nikon-d5300.json` precedes `nikon_d5300.json`.
        let first = CameraProfile::new("nikon d5300", None, 1, 100, Mat3::IDENTITY, None);
        save(&first, &dir).unwrap();
        let second = CameraProfile::new("nikon-d5300", None, 2, 100, Mat3::IDENTITY, None);
        save(&second, &dir).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let dir_for_lookup = dir.clone();
        std::thread::spawn(move || {
            let found = get_from("NIKON D5300", None, Some(&dir_for_lookup));
            let _ = tx.send(found.map(|profile| profile.files));
        });

        let files = rx.recv_timeout(std::time::Duration::from_secs(1)).expect("local profile lookup blocked on a special file");
        assert_eq!(files, Some(2), "local profile selection must not depend on filesystem iteration order");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
