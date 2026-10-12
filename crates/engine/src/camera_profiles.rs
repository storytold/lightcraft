//! Camera colour profiles of our own, pooled from many photos of one camera model.
//!
//! A raw file without colour matrices (Sony ARW, Nikon NEF, Fujifilm RAF, Canon CRW) gets its look fitted to its own embedded camera
//! JPEG (`camera_preview`), but one photo shows too little of some colours: a lime shirt covering a
//! few dozen proxy pixels next to a hillside of foliage at the same hue. `lightcraft-cli calibrate`
//! pools the colour pairs of many photos per model and fits one matrix and hue/saturation/value
//! table; photos of that model then only fit their tone and chroma curves.
//!
//! Profiles are JSON files (`<model>.json`) in [`dir`]: `LIGHTCRAFT_CAMERA_PROFILES`, else
//! `<config>/camera-profiles`; a local profile replaces the one built in ([`BUNDLED`], from
//! `assets/camera-profiles/`). They are read once per process for the folder they are looked up
//! in; a damaged or hostile file is ignored with a warning. They hold aggregate colour statistics
//! only, never image content.
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
/// `(model, JSON)`.
pub const BUNDLED: &[(&str, &str)] = &[
    ("Canon EOS D60", include_str!("../../../assets/camera-profiles/Canon_EOS_D60.json")),
    ("ILCA-99M2", include_str!("../../../assets/camera-profiles/ILCA-99M2.json")),
    ("ILCE-7CR", include_str!("../../../assets/camera-profiles/ILCE-7CR.json")),
    ("ILCE-7M4", include_str!("../../../assets/camera-profiles/ILCE-7M4.json")),
    ("ILCE-7RM5", include_str!("../../../assets/camera-profiles/ILCE-7RM5.json")),
    ("NIKON D5300", include_str!("../../../assets/camera-profiles/NIKON_D5300.json")),
    ("NIKON Z 6_2", include_str!("../../../assets/camera-profiles/NIKON_Z_6_2.json")),
    ("NIKON Z 8", include_str!("../../../assets/camera-profiles/NIKON_Z_8.json")),
    ("X-H2S", include_str!("../../../assets/camera-profiles/X-H2S.json")),
    ("X-T4", include_str!("../../../assets/camera-profiles/X-T4.json")),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraProfile {
    pub version: u32,
    /// The camera model as the files report it (Exif `Model`), e.g. `ILCE-7M4`.
    pub model: String,
    /// Photos and colour pairs the profile was fitted on.
    pub files: usize,
    pub samples: usize,
    /// White-balanced camera RGB (with the baseline exposure) → linear Rec.2020, rows.
    matrix: [[f64; 3]; 3],
    /// Hue/saturation/value correction after `matrix` (linear ProPhoto RGB).
    pub hue_sat: Option<HsvTable>,
}

impl CameraProfile {
    pub fn new(model: &str, files: usize, samples: usize, matrix: Mat3, hue_sat: Option<HsvTable>) -> CameraProfile {
        CameraProfile { version: VERSION, model: model.to_owned(), files, samples, matrix: matrix.0, hue_sat }
    }

    pub fn matrix(&self) -> Mat3 {
        Mat3(self.matrix)
    }

    /// Whether the data is usable (bounded matrix, table shape matching its data).
    fn valid(&self) -> bool {
        let matrix = self.matrix.iter().flatten().all(|v| v.is_finite() && v.abs() < 8.0) && Mat3(self.matrix).inverse().is_some();
        let table = self.hue_sat.as_ref().is_none_or(|t| {
            let dims = (1..=4096).contains(&t.hue_divisions) && (2..=4096).contains(&t.sat_divisions) && (1..=4096).contains(&t.val_divisions);
            let len = t.hue_divisions.checked_mul(t.sat_divisions).and_then(|n| n.checked_mul(t.val_divisions));
            dims && len == Some(t.data.len()) && t.data.iter().flatten().all(|v| v.is_finite()) && t.data.iter().all(|e| e[1] >= 0.0 && e[2] >= 0.0)
        });
        self.version == VERSION && !self.model.is_empty() && matrix && table
    }
}

/// LightCraft's configuration folder (settings, GPU marker, camera profiles); one helper for the app.
pub use crate::config::config_dir;

/// Where camera profiles are read from and written to.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("LIGHTCRAFT_CAMERA_PROFILES").map(PathBuf::from).or_else(|| config_dir().map(|d| d.join("camera-profiles")))
}

/// File name of `model`'s profile: letters, digits, `-` and `_` kept, anything else `_`.
pub fn file_name(model: &str) -> String {
    let name: String = model.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("{name}.json")
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

/// The built-in profile for `model`, if any.
fn bundled(model: &str) -> Option<CameraProfile> {
    let needle = normalized_model(model);
    let needle =
        ALIASES.iter().find(|(spelling, _)| normalized_model(spelling) == needle).map_or(needle, |(_, canonical)| normalized_model(canonical));
    let (_, json) = BUNDLED.iter().find(|(m, _)| normalized_model(m) == needle)?;
    parse(json.as_bytes(), &format!("built-in profile {model}")).inspect_err(|e| eprintln!("lightcraft: ignoring camera profile {e}")).ok()
}

/// Write `profile` to `dir` (created if needed) as `<model>.json`; returns the path.
pub fn save(profile: &CameraProfile, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(file_name(&profile.model));
    let json = serde_json::to_vec_pretty(profile).map_err(|e| e.to_string())?;
    // a fresh temp file renamed over the profile: never a fixed `.tmp` name, which a planted link
    // would have written through
    lightcraft_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Profiles already read, per folder they were read from and model: a lookup pointed at another
/// folder (a test's temporary directory) must not return the profile read from this one.
type Cache = HashMap<(Option<PathBuf>, String), Option<Arc<CameraProfile>>>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The profile for camera `model`: from [`dir`], else built in; read once per process.
pub fn get(model: &str) -> Option<Arc<CameraProfile>> {
    get_from(model, dir().as_deref())
}

/// The profile for camera `model` read from `directory`, else built in; read once per process for
/// that folder.
///
/// `directory` is the profile folder to search for a local profile; [`dir`] resolves it from
/// [`LIGHTCRAFT_CAMERA_PROFILES`] or the config folder. Separating it lets tests point the lookup
/// at a temporary directory without changing process environment variables.
fn get_from(model: &str, directory: Option<&Path>) -> Option<Arc<CameraProfile>> {
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
    let key = (directory.map(Path::to_path_buf), model.to_owned());
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    // Scan the directory for a local profile. Candidates are regular .json files
    // ranked so a normalized stem match (case, whitespace and separator folding)
    // beats a model-name match; within a rank the sorted path order decides.
    // Selection therefore never depends on read_dir iteration order, and only
    // regular files are loaded, so a FIFO or directory named like a profile
    // cannot block while the cache mutex is held.
    let profile = {
        let needle = normalize_for_local_match(model);
        directory.and_then(|d| {
            let mut entries: Vec<_> = std::fs::read_dir(d).ok()?.flatten().collect();
            entries.sort_by_key(|e| e.path());
            let mut ranked: Vec<(u8, PathBuf)> = Vec::new();
            for e in &entries {
                let path = e.path();
                if !path.extension().is_some_and(|x| x == "json") || !path.is_file() {
                    continue;
                }
                let rank = if path.file_stem().is_some_and(|s| normalize_for_local_match(&s.to_string_lossy()) == needle) { 1 } else { 2 };
                ranked.push((rank, path));
            }
            ranked.sort();
            for (_, path) in ranked {
                if let Some(p) = load(&path).ok().filter(|p| normalize_for_local_match(&p.model) == needle) {
                    return Some(Arc::new(p));
                }
            }
            None
        })
    };
    let profile = profile.or_else(|| bundled(model).map(Arc::new));
    cache.insert(key, profile.clone());
    profile
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
    for (model, json) in BUNDLED {
        h.str(model).str(json);
    }
    h.finish().0 as u64
}

/// One colour pair: white-balanced camera RGB, and the camera JPEG's linear Rec.2020 colour.
pub type Pair = ([f64; 3], [f64; 3]);

/// The colour pairs of one photo, as pooled for a fit and as shared in a pairs file
/// (`lightcraft-cli calibrate --pairs`, see [`crate::camera_pairs`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoPairs {
    /// The camera model as the file reports it (Exif `Model`).
    pub model: String,
    /// Identifies the photo for de-duplication: a 128-bit hash of the raw file's bytes (32 hex digits). It is
    /// not the file name, path or any metadata.
    pub id: String,
    /// Sony's Dynamic Range Optimizer was on for the photo (`None`: the file doesn't say).
    pub dro: Option<bool>,
    pub pairs: Vec<Pair>,
}

/// The photos read, and their colour pairs, pooled per camera model by [`Pool::fit`].
#[derive(Default)]
pub struct Pool {
    photos: Vec<PhotoPairs>,
}

/// Most colour pairs kept per photo, so a few busy photos can't dominate a profile.
pub(crate) const PAIRS_PER_FILE: usize = 4000;

/// Pairs are held at single precision, so pairs read back from a pairs file are bit-for-bit those
/// sampled from the raw and the same profile comes out of either.
fn single(pair: Pair) -> Pair {
    let f = |v: [f64; 3]| v.map(|x| f64::from(x as f32));
    (f(pair.0), f(pair.1))
}

impl Pool {
    /// Add one raw file's colour pairs. `Ok(None)` when the file can't contribute (not an ARW,
    /// NEF or RAF without colour matrices, no usable camera JPEG, too little colour).
    pub fn add(&mut self, bytes: &[u8]) -> Result<Option<String>, String> {
        let mut raw = lightcraft_raw::decode(bytes).map_err(|e| e.to_string())?;
        // colour only: geometric lens corrections would stop the sensor proxy from binning (the pairs still follow
        // the warp the camera applied to its JPEG)
        let lens = crate::camera_preview::lens_geometry(&raw.opcodes.list3);
        raw.opcodes.list3.retain(|op| !op.is_lens_correction());
        let Some(model) = raw.metadata.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) else { return Ok(None) };
        let Some(pairs) = crate::camera_preview::profile_pairs(&raw, bytes, lens.as_ref()) else { return Ok(None) };
        let step = pairs.len().div_ceil(PAIRS_PER_FILE).max(1);
        self.photos.push(PhotoPairs {
            model: model.to_owned(),
            id: lightcraft_preview::hash_bytes(bytes).to_string(),
            dro: lightcraft_raw::embedded_preview_dynamic_range_optimized(bytes),
            pairs: pairs.into_iter().step_by(step).map(single).collect(),
        });
        Ok(Some(model.to_owned()))
    }

    /// Add photos read from a pairs file.
    pub fn add_photos(&mut self, photos: Vec<PhotoPairs>) {
        self.photos.extend(photos);
    }

    /// The pairs file ([`crate::camera_pairs`]) of every photo in the pool.
    pub fn pairs_file(&self) -> Result<Vec<u8>, String> {
        crate::camera_pairs::write(&self.photos())
    }

    /// Add the photos of a pairs file; returns how many it held.
    pub fn add_pairs_file(&mut self, bytes: &[u8], origin: &str) -> Result<usize, String> {
        let photos = crate::camera_pairs::read(bytes, origin)?;
        let n = photos.len();
        self.add_photos(photos);
        Ok(n)
    }

    /// The photos in a fixed order (model, then id) with each id once, so the same photos give the same fit
    /// however they arrived (raw files in any thread order, pairs files, both).
    pub fn photos(&self) -> Vec<&PhotoPairs> {
        let mut photos: Vec<&PhotoPairs> = self.photos.iter().collect();
        photos.sort_by(|a, b| (&a.model, &a.id).cmp(&(&b.model, &b.id)));
        photos.dedup_by(|a, b| a.model == b.model && a.id == b.id);
        photos
    }

    /// Fit a profile per model with at least `min_files` photos.
    pub fn fit(&self, min_files: usize) -> Vec<Result<CameraProfile, String>> {
        let photos = self.photos();
        let mut models: Vec<(&str, usize, Vec<Pair>)> = Vec::new();
        for photo in photos {
            if models.last().is_none_or(|m| m.0 != photo.model) {
                models.push((&photo.model, 0, Vec::new()));
            }
            let last = models.last_mut().expect("just pushed");
            last.1 += 1;
            last.2.extend_from_slice(&photo.pairs);
        }
        models
            .into_iter()
            .filter(|(_, files, _)| *files >= min_files)
            .map(|(model, files, pairs)| {
                let (matrix, hue_sat) = crate::camera_preview::fit_profile(&pairs).ok_or_else(|| format!("{model}: no usable colour fit"))?;
                Ok(CameraProfile::new(model, files, pairs.len(), matrix, hue_sat))
            })
            .collect()
    }

    /// Add another pool's photos and pairs (pools filled on separate threads).
    pub fn merge(&mut self, other: Pool) {
        self.photos.extend(other.photos);
    }

    /// Photos read per model.
    pub fn files(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = Vec::new();
        for photo in self.photos() {
            match v.last_mut() {
                Some(last) if last.0 == photo.model => last.1 += 1,
                _ => v.push((photo.model.clone(), 1)),
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> CameraProfile {
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[5.0, 1.2, 1.0]; 8], srgb_value: false };
        CameraProfile::new("ILCE-7M4", 12, 3456, Mat3([[1.6, -0.5, -0.1], [-0.2, 1.3, -0.1], [-0.1, -0.2, 1.3]]), Some(table))
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
    fn bundled_profiles_are_valid_and_named_after_their_model() {
        assert!(!BUNDLED.is_empty());
        for (model, json) in BUNDLED {
            let p = parse(json.as_bytes(), model).unwrap();
            assert_eq!(p.model, *model);
            assert!(p.files >= 5 && p.hue_sat.is_some(), "{model}: {} photos", p.files);
            assert_eq!(bundled(model), Some(p));
        }
        assert!(bundled("No Such Camera").is_none());
        // a model name with spaces is stored under its safe file name
        assert!(bundled("Canon EOS D60").is_some_and(|p| p.files > 1000));
        assert_eq!(file_name("Canon EOS D60"), "Canon_EOS_D60.json");
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
            assert!(bundled(canonical).is_some(), "the bundled {canonical:?} profile must load");
            assert_eq!(bundled(variant), bundled(canonical), "NEF spelling {variant:?} must find the bundled {canonical:?} profile");
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
        assert!(bundled("Nikon Z 6-2").is_some(), "Nikon Z 6-2 should match bundled NIKON Z 6_2 profile");
        assert!(bundled("Nikon Z 6 2").is_some(), "Nikon Z 6 2 should match bundled NIKON Z 6_2 profile");
        // Z8 already works via ALIASES
        assert_eq!(bundled("Nikon Z8"), bundled("NIKON Z 8"), "Nikon Z8 should continue to match via ALIASES");

        // After fix: local profile lookup should use normalized matching
        // This would make file_name consistent for variants
        // assert_eq!(file_name("Nikon Z8"), file_name("NIKON Z 8"));

        // Staleness test uses fixed version 13 (tested in smart/tests.rs)
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
        assert_eq!(file_name("ILCE-7M4"), "ILCE-7M4.json");
        assert_eq!(file_name(" DSC-RX100M3 "), "DSC-RX100M3.json");
        assert_eq!(file_name("../../etc/passwd"), "______etc_passwd.json");
        assert_eq!(file_name("Ω 1/2"), "__1_2.json");
    }

    /// Synthetic photos of one model: pairs from a fixed colour mapping with a little structure.
    fn synthetic(model: &str, n: usize, seed: u32) -> Vec<PhotoPairs> {
        let mut state = seed;
        let mut next = move || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            f64::from(state >> 8) / f64::from(1u32 << 24)
        };
        (0..n)
            .map(|k| PhotoPairs {
                model: model.into(),
                id: format!("{:032x}", u128::from(seed) * 1000 + k as u128 + 1),
                dro: (k % 2 == 0).then_some(k % 4 == 0),
                pairs: (0..300)
                    .map(|_| {
                        let c = [next() * 0.8 + 0.02, next() * 0.8 + 0.02, next() * 0.8 + 0.02];
                        let y = [c[0] * 1.5 - c[1] * 0.3 - c[2] * 0.1, c[1] * 1.3 - c[0] * 0.1, c[2] * 1.4 - c[1] * 0.2 + 0.05 * c[0]]
                            .map(|v| v.clamp(0.001, 1.0));
                        single((c, y))
                    })
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn pairs_files_pool_to_the_same_profile_whatever_the_order_or_split() {
        let photos = synthetic("ILCE-7M4", 8, 7);
        let mut direct = Pool::default();
        direct.add_photos(photos.clone());
        let direct = direct.fit(5).remove(0).unwrap();
        assert_eq!((direct.files, direct.samples), (8, 2400));
        // two contributors' files, one of them listing a photo twice, pooled in the other order
        let (first, second) = photos.split_at(3);
        let write = |p: &[PhotoPairs]| crate::camera_pairs::write(&p.iter().collect::<Vec<_>>()).unwrap();
        let (a, b) = (write(first), write(second));
        let mut pooled = Pool::default();
        assert_eq!(pooled.add_pairs_file(&b, "b").unwrap(), 5);
        assert_eq!(pooled.add_pairs_file(&a, "a").unwrap(), 3);
        assert_eq!(pooled.add_pairs_file(&a, "a again").unwrap(), 3);
        assert_eq!(pooled.files(), vec![("ILCE-7M4".to_owned(), 8)]);
        assert_eq!(pooled.fit(5).remove(0).unwrap(), direct);
        // a pool re-exported as a pairs file holds each photo once
        let again = pooled.pairs_file().unwrap();
        let mut reread = Pool::default();
        assert_eq!(reread.add_pairs_file(&again, "again").unwrap(), 8);
        assert_eq!(reread.fit(5).remove(0).unwrap(), direct);
        // too few photos of a model are left out
        pooled.add_photos(synthetic("X-T4", 2, 9));
        assert_eq!(pooled.fit(5).len(), 1);
    }

    #[test]
    fn local_profile_lookup_uses_normalized_matching() {
        // Local profiles saved under one spelling should be found by another
        // spelling that normalizes to the same key.
        let dir = std::env::temp_dir().join(format!("lc-local-profile-lookup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Save a profile under the canonical spelling
        let profile = CameraProfile::new("NIKON Z 6_2", 10, 100, Mat3::IDENTITY, None);
        let path = save(&profile, &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "NIKON_Z_6_2.json");

        // Point the lookup at the temporary directory so it finds the local profile
        // instead of the bundled one.
        let found = get_from("Nikon Z 6-2", Some(&dir));
        assert!(found.is_some(), "Nikon Z 6-2 should find local NIKON Z 6_2 profile");
        assert_eq!(found.as_ref().unwrap().model, "NIKON Z 6_2");
        assert_eq!(found.as_ref().unwrap().files, 10, "the local NIKON Z 6_2 profile, not the bundled one, must be used");

        let found = get_from("Nikon Z 6 2", Some(&dir));
        assert!(found.is_some(), "Nikon Z 6 2 should find local NIKON Z 6_2 profile");
        assert_eq!(found.as_ref().unwrap().model, "NIKON Z 6_2");
        assert_eq!(found.as_ref().unwrap().files, 10, "the local NIKON Z 6_2 profile, not the bundled one, must be used");

        let found = get_from("NIKON Z 6-2", Some(&dir));
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

        let local = CameraProfile::new("NIKON Z 8", 1, 100, Mat3::IDENTITY, None);
        let path = save(&local, &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "NIKON_Z_8.json");

        let found = get_from("NIKON Z8", Some(&dir));
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

        let local = CameraProfile::new("NIKON Z8", 2, 100, Mat3::IDENTITY, None);
        let path = save(&local, &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "NIKON_Z8.json");

        let found = get_from("NIKON Z 8", Some(&dir)).expect("a NIKON Z 8 profile");
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
        let first = CameraProfile::new("nikon d5300", 1, 100, Mat3::IDENTITY, None);
        save(&first, &dir).unwrap();
        let second = CameraProfile::new("nikon-d5300", 2, 100, Mat3::IDENTITY, None);
        save(&second, &dir).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let dir_for_lookup = dir.clone();
        std::thread::spawn(move || {
            let found = get_from("NIKON D5300", Some(&dir_for_lookup));
            let _ = tx.send(found.map(|profile| profile.files));
        });

        let files = rx.recv_timeout(std::time::Duration::from_secs(1)).expect("local profile lookup blocked on a special file");
        assert_eq!(files, Some(2), "local profile selection must not depend on filesystem iteration order");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
