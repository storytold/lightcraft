//! Camera colour profiles of our own, pooled from many photos of one camera model.
//!
//! A raw file without colour matrices (Sony ARW, Nikon NEF, Fujifilm RAF) gets its look fitted to its own embedded camera
//! JPEG (`camera_preview`), but one photo shows too little of some colours: a lime shirt covering a
//! few dozen proxy pixels next to a hillside of foliage at the same hue. `lightcraft-cli calibrate`
//! pools the colour pairs of many photos per model and fits one matrix and hue/saturation/value
//! table; photos of that model then only fit their tone and chroma curves.
//!
//! Profiles are JSON files (`<model>.json`) in [`dir`]: `LIGHTCRAFT_CAMERA_PROFILES`, else
//! `<config>/camera-profiles`; a local profile replaces the one built in ([`BUNDLED`], from
//! `assets/camera-profiles/`). They are read once per process; a damaged or hostile file is
//! ignored with a warning. They hold aggregate colour statistics only, never image content.
use lightcraft_color::Mat3;
use lightcraft_raw::profile::HsvTable;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Format version of the profile files.
const VERSION: u32 = 1;
/// Largest profile file read (a 5 × 72 × 5 table is ~60 KB of JSON).
const MAX_FILE: u64 = 4 << 20;

/// Profiles built into LightCraft (`assets/camera-profiles/`, see `assets/ATTRIBUTION.md`):
/// `(model, JSON)`.
pub const BUNDLED: &[(&str, &str)] = &[
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

/// Canonical model key shared by metadata, pairs and profile lookup. Case and interior
/// whitespace are significant: distinct Exif model strings must stay distinct.
pub(crate) fn model_key(model: &str) -> &str {
    model.trim()
}

/// File name of `model`'s profile: letters, digits, `-` and `_` kept, anything else `_`.
pub fn file_name(model: &str) -> String {
    let name: String = model_key(model).chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
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

/// The built-in profile for `model`, if any.
fn bundled(model: &str) -> Option<CameraProfile> {
    let (_, json) = BUNDLED.iter().find(|(m, _)| *m == model)?;
    parse(json.as_bytes(), &format!("built-in profile {model}")).inspect_err(|e| eprintln!("lightcraft: ignoring camera profile {e}")).ok()
}

/// Write `profile` to `dir` (created if needed) as `<model>.json`; returns the path.
pub fn save(profile: &CameraProfile, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(file_name(&profile.model));
    let json = serde_json::to_vec_pretty(profile).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn cache() -> &'static Mutex<HashMap<String, Option<Arc<CameraProfile>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<CameraProfile>>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The profile for camera `model`: from [`dir`], else built in; read once per process.
pub fn get(model: &str) -> Option<Arc<CameraProfile>> {
    let model = model_key(model);
    if model.is_empty() {
        return None;
    }
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = cache.get(model) {
        return hit.clone();
    }
    let path = dir().map(|d| d.join(file_name(model)));
    let profile = match path {
        Some(path) if path.is_file() => match load(&path) {
            Ok(p) if model_key(&p.model) == model => Some(Arc::new(p)),
            Ok(p) => {
                eprintln!("lightcraft: ignoring camera profile {}: it is for {:?}, not {model:?}", path.display(), p.model);
                None
            }
            Err(e) => {
                eprintln!("lightcraft: ignoring camera profile {e}");
                None
            }
        },
        _ => None,
    };
    let profile = profile.or_else(|| bundled(model).map(Arc::new));
    cache.insert(model.to_owned(), profile.clone());
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

/// Puts a photo's pairs in value order (all six values, compared in turn): pairs are sampled in scan order over the
/// preview, and that order would keep the photo's coarse layout in a pairs file. Pairs are ordered when they enter the
/// pool, from a raw file or a pairs file, so either route gives the same fit (the fits hold out every third pair by
/// position, so they follow the order).
pub fn canonical_order(pairs: &mut [Pair]) {
    let key = |p: &Pair| -> [f64; 6] { [p.0[0], p.0[1], p.0[2], p.1[0], p.1[1], p.1[2]] };
    pairs.sort_by(|a, b| key(a).iter().zip(&key(b)).map(|(x, y)| x.total_cmp(y)).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal));
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
        let Some(model) = raw.metadata.model.as_deref().map(model_key).filter(|m| !m.is_empty()) else { return Ok(None) };
        let Some(pairs) = crate::camera_preview::profile_pairs(&raw, bytes, lens.as_ref()) else { return Ok(None) };
        // subsampled in scan order (evenly over the frame), then put in value order
        let step = pairs.len().div_ceil(PAIRS_PER_FILE).max(1);
        let mut pairs: Vec<Pair> = pairs.into_iter().step_by(step).map(single).collect();
        canonical_order(&mut pairs);
        self.photos.push(PhotoPairs {
            model: model.to_owned(),
            id: lightcraft_preview::hash_bytes(bytes).to_string(),
            dro: lightcraft_raw::embedded_preview_dynamic_range_optimized(bytes),
            pairs,
        });
        Ok(Some(model.to_owned()))
    }

    /// Add photos read from a pairs file (their pairs put in [`canonical_order`]).
    pub fn add_photos(&mut self, mut photos: Vec<PhotoPairs>) {
        for photo in &mut photos {
            photo.model = model_key(&photo.model).to_owned();
            canonical_order(&mut photo.pairs);
        }
        self.photos.extend(photos);
    }

    /// The pairs file ([`crate::camera_pairs`]) of every photo in the pool.
    pub fn pairs_file(&self) -> Result<Vec<u8>, String> {
        crate::camera_pairs::write(&self.photos())
    }

    /// Add the photos of a pairs file; returns how many it held. A photo already in the pool (same model and id) is
    /// skipped, so importing a file again changes nothing. Refuse an import that would leave more than
    /// [`crate::camera_pairs::MAX_PHOTOS`] distinct photos; each holds at most [`PAIRS_PER_FILE`] pairs.
    pub fn add_pairs_file(&mut self, bytes: &[u8], origin: &str) -> Result<usize, String> {
        let photos = crate::camera_pairs::read(bytes, origin)?;
        let n = photos.len();
        let mut seen: HashSet<(String, String)> = self.photos.iter().map(|p| (p.model.clone(), p.id.clone())).collect();
        let new: Vec<PhotoPairs> = photos.into_iter().filter(|p| seen.insert((p.model.clone(), p.id.clone()))).collect();
        if seen.len() > crate::camera_pairs::MAX_PHOTOS {
            return Err(format!("{origin}: {} photos is more than a pairs pool may hold ({})", seen.len(), crate::camera_pairs::MAX_PHOTOS));
        }
        self.add_photos(new);
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
    fn metadata_pairs_and_profile_lookup_share_model_keys() {
        let model = BUNDLED[0].0;
        let padded = format!(" \t{model}\r\n");
        assert_eq!(model_key(&padded), model);
        assert_eq!(file_name(&padded), file_name(model));
        assert_eq!(get(&padded), get(model));
        assert!(get(model).is_some());
        let photos = synthetic(&padded, 5, 7);
        let mut pool = Pool::default();
        pool.add_photos(photos.clone());
        assert_eq!(pool.files(), [(model.to_owned(), 5)]);
        let bytes = crate::camera_pairs::write(&photos.iter().collect::<Vec<_>>()).unwrap();
        let mut imported = Pool::default();
        imported.add_pairs_file(&bytes, "test").unwrap();
        assert_eq!(imported.files(), pool.files());
        assert_eq!(imported.photos(), pool.photos());
        assert_eq!(imported.add_pairs_file(&bytes, "repeat").unwrap(), 5);
        assert_eq!(imported.files(), pool.files());
        assert_ne!(model_key("Model A"), model_key("model a"));
        assert_ne!(model_key("Model  A"), model_key("Model A"));
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
    fn pairs_files_enforce_cumulative_photo_limit() {
        let photo = |i: usize| PhotoPairs { model: "ILCE-7M4".into(), id: format!("{i:032x}"), dro: None, pairs: vec![([0.1; 3], [0.2; 3])] };
        let file = |i: usize| crate::camera_pairs::write(&[&photo(i)]).unwrap();
        let max = crate::camera_pairs::MAX_PHOTOS;
        let mut pool = Pool::default();
        pool.add_photos((0..max - 1).map(photo).collect());
        // a photo the pool already holds is skipped, however often it's imported
        for _ in 0..3 {
            assert_eq!(pool.add_pairs_file(&file(0), "again").unwrap(), 1);
        }
        assert_eq!(pool.photos.len(), max - 1);
        assert_eq!(pool.add_pairs_file(&file(max - 1), "at limit").unwrap(), 1);
        assert_eq!(pool.photos.len(), max);
        assert_eq!(pool.add_pairs_file(&file(0), "known at limit").unwrap(), 1, "a known photo still imports at the limit");
        let before = pool.photos.clone();
        let error = pool.add_pairs_file(&file(max), "over limit").unwrap_err();
        assert!(error.contains("over limit: 2001 photos is more than a pairs pool may hold (2000)"), "{error}");
        assert_eq!(pool.photos, before, "rejection must leave the stored pool unchanged");
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
    fn pairs_files_keep_no_scan_order_and_fit_the_same() {
        // pairs sampled in scan order over a 192x128 gradient preview: red follows the column, green the row
        let (w, h) = (192, 128);
        let scan: Vec<Pair> = (0..w * h)
            .step_by(7)
            .map(|i| {
                let (x, y) = ((i % w) as f64 / w as f64, (i / w) as f64 / h as f64);
                let c = [0.05 + 0.8 * x, 0.05 + 0.8 * y, 0.4 - 0.3 * x * y];
                single((c, [c[0] * 1.4 - c[1] * 0.2, c[1] * 1.3 - c[2] * 0.1, c[2] * 1.2 + 0.05 * c[0]]))
            })
            .collect();
        let rows = |p: &[Pair]| p.windows(2).filter(|w| w[1].0[1] < w[0].0[1]).count();
        assert_eq!(rows(&scan), 0, "in scan order the row channel never steps back");
        let photos: Vec<PhotoPairs> =
            (0..5).map(|k| PhotoPairs { model: "ILCE-7M4".into(), id: format!("{:032x}", k + 1), dro: None, pairs: scan.clone() }).collect();
        let mut pool = Pool::default();
        pool.add_photos(photos.clone());
        let fitted = pool.fit(5).remove(0).unwrap();
        // the written file holds each photo's pairs sorted by value, not in scan order
        let written = crate::camera_pairs::write(&photos.iter().collect::<Vec<_>>()).unwrap();
        assert_eq!(written, pool.pairs_file().unwrap());
        let reread = crate::camera_pairs::read(&written, "t").unwrap();
        for photo in &reread {
            assert_eq!(photo.pairs.len(), scan.len());
            assert_ne!(photo.pairs, scan);
            assert!(photo.pairs.is_sorted_by(|a, b| a.0[0] <= b.0[0]), "sorted by value");
            assert!(rows(&photo.pairs) >= w / 2, "the row channel no longer follows the scan");
            let mut a = photo.pairs.clone();
            let mut b = scan.clone();
            a.sort_by(|p, q| p.partial_cmp(q).unwrap());
            b.sort_by(|p, q| p.partial_cmp(q).unwrap());
            assert_eq!(a, b, "the same pairs");
        }
        // a round trip, or the pairs arriving in any order, gives the same profile
        let mut again = Pool::default();
        again.add_pairs_file(&written, "t").unwrap();
        assert_eq!(again.fit(5).remove(0).unwrap(), fitted);
        let mut reversed = photos;
        for photo in &mut reversed {
            photo.pairs.reverse();
        }
        let mut shuffled = Pool::default();
        shuffled.add_photos(reversed);
        assert_eq!(shuffled.fit(5).remove(0).unwrap(), fitted);
    }

    #[test]
    fn pool_skips_files_that_cannot_contribute() {
        let mut pool = Pool::default();
        assert!(pool.add(b"not a raw file").is_err());
        assert!(pool.fit(1).is_empty());
        assert!(pool.files().is_empty());
    }
}
