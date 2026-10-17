//! Lens profile corrections from a lens database the user supplies ([`lcc_lensdb`]).
//!
//! When a raw carries no lens correction of its own (DNG opcodes, or a camera correction a raw reader converts,
//! [`crate::files::embedded_lens`]), the lens named in its EXIF is looked up in a lensfun-format database (e.g.
//! lensfun's `data/db`, CC-BY-SA 3.0, which is never bundled with the app). Where it comes from, first match wins:
//! the directory in `{ENV_PREFIX}_LENSFUN_DIR`, else the folder chosen in Settings ▸ Lens profiles (or downloaded
//! there on request, [`crate::lens_download`]). The profile is converted to the same [`lcc_develop::EmbeddedLens`]
//! form, so "Enable Profile Corrections", its amount sliders, the GPU path and DNG export treat both alike.
//!
//! The loaded database is cached process-wide and swapped when the folder changes ([`set_dir`], [`reload`]): each
//! change bumps [`generation`], so sessions know to look their photos' lenses up again.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use lcc_develop::EmbeddedLens;
use lcc_lensdb::{LensDb, Shot};

/// The environment variable (after the app prefix) naming the lens database directory. It overrides the folder
/// chosen in Settings.
pub const ENV: &str = "LENSFUN_DIR";

/// Where the database in use comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// `{ENV_PREFIX}_LENSFUN_DIR`.
    Env,
    /// The folder chosen (or downloaded into) in Settings.
    Folder,
}

struct Cache {
    /// The folder chosen in Settings.
    dir: Option<PathBuf>,
    /// The database loaded for the current source (`Some(None)`: none configured, or it holds no lenses).
    loaded: Option<Option<Arc<LensDb>>>,
    /// Bumped on every change of source or contents.
    generation: u64,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { dir: None, loaded: None, generation: 0 });

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The environment override, when set and nonempty.
fn env_dir() -> Option<PathBuf> {
    lcc_brand::env_os(ENV).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The database folder in use and where it comes from (the environment wins over Settings).
pub fn source() -> Option<(PathBuf, Origin)> {
    env_dir().map(|d| (d, Origin::Env)).or_else(|| cache().dir.clone().map(|d| (d, Origin::Folder)))
}

/// The folder chosen in Settings (whether or not the environment overrides it).
pub fn configured_dir() -> Option<PathBuf> {
    cache().dir.clone()
}

/// Use `dir` (or no folder) as the Settings database; the next lookup loads it.
pub fn set_dir(dir: Option<PathBuf>) {
    let mut c = cache();
    c.dir = dir;
    c.loaded = None;
    c.generation = c.generation.wrapping_add(1);
}

/// Read the database again (its files changed, e.g. after a download).
pub fn reload() {
    let mut c = cache();
    c.loaded = None;
    c.generation = c.generation.wrapping_add(1);
}

/// Changes with every [`set_dir`] / [`reload`].
pub fn generation() -> u64 {
    cache().generation
}

/// Load the lensfun-format files in `dir`; `None` when it can't be read or holds no lenses.
pub fn load(dir: &Path) -> Option<LensDb> {
    match LensDb::load_dir(dir) {
        Ok((db, errors)) => {
            for (file, e) in errors {
                log::warn!("lens database: skipped {file}: {e}");
            }
            (!db.lenses.is_empty()).then_some(db)
        }
        Err(e) => {
            log::warn!("lens database: {e}");
            None
        }
    }
}

/// The database in use, loaded on first use after a change; `None` when none is configured or it holds no lenses.
pub fn database() -> Option<Arc<LensDb>> {
    let env = env_dir();
    let mut c = cache();
    if let Some(db) = &c.loaded {
        return db.clone();
    }
    let dir = env.or_else(|| c.dir.clone());
    // loaded under the lock: a second caller waits for this load instead of repeating it
    let db = dir.as_deref().and_then(load).map(Arc::new);
    c.loaded = Some(db.clone());
    db
}

/// What the lens database says about a photo's lens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    /// No database is set up (or it holds no lenses).
    NoDatabase,
    /// Lens profiles apply to raw photos only.
    NotRaw,
    /// The photo's metadata names no lens.
    NoLensName,
    /// The database has no profile for the lens (its name): the one named in the metadata, or the chosen one.
    NotFound(String),
    /// The database's profile matched by the lens name in the metadata (its name).
    Found(String),
    /// The database's profile for the lens given by the caller ([`GivenLens`], its name).
    Chosen(String),
}

/// Most characters of a given lens name (longer names are refused, never matched).
pub const MAX_NAME: usize = 256;

/// A lens stated by the caller instead of read from the file (a lens picker, an adapted or manual lens, stripped
/// metadata), plus the focal length and aperture such files usually lack. It wins over the automatic match by name,
/// but not over a correction the file carries itself ([`resolve_raw`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GivenLens {
    /// The lens maker as the database names it (optional; narrows the match).
    pub maker: Option<String>,
    /// The lens model: one of a profile's names ([`LensDb`]'s `models`), as `lens.db.list` reports them.
    pub model: String,
    /// Focal length, mm; the file's when `None`.
    pub focal: Option<f64>,
    /// F-number; the file's when `None`.
    pub aperture: Option<f64>,
}

impl GivenLens {
    /// Check a lens given from outside (a command or agent): a nonempty name of at most [`MAX_NAME`] characters,
    /// a finite focal length in 0–10000 mm and an f-number in 0.5–256.
    pub fn validate(&self) -> std::result::Result<(), String> {
        let ok_name = |n: &str| n.chars().count() <= MAX_NAME;
        if self.model.trim().is_empty() {
            return Err("the lens model is empty".into());
        }
        if !ok_name(&self.model) || !self.maker.as_deref().is_none_or(ok_name) {
            return Err(format!("lens names are at most {MAX_NAME} characters"));
        }
        if self.focal.is_some_and(|f| !(f.is_finite() && f > 0.0 && f <= 10_000.0)) {
            return Err("focal must be a focal length in mm (0–10000)".into());
        }
        if self.aperture.is_some_and(|a| !(a.is_finite() && (0.5..=256.0).contains(&a))) {
            return Err("aperture must be an f-number (0.5–256)".into());
        }
        Ok(())
    }
}

/// Which correction applies to a photo ([`resolve_raw`]), in this order: the file's own, a lens given by the
/// caller, the automatic match by the lens name in the metadata (none: no correction).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The file's embedded correction (DNG opcodes, a converted camera correction).
    Embedded,
    /// The database profile of the given lens (its name).
    Chosen(String),
    /// The database profile matched by the metadata's lens name (its name).
    Found(String),
}

fn profile_name(p: &lcc_lensdb::Profile, fallback: &str) -> String {
    p.models.first().cloned().unwrap_or_else(|| fallback.trim().to_string())
}

/// The profile for a given lens: one whose name (and maker, when given) equals it, ignoring case and outer spaces,
/// else the best match by name ([`LensDb::find_lens`]).
pub fn find_given<'a>(db: &'a LensDb, given: &GivenLens) -> Option<&'a lcc_lensdb::Profile> {
    let model = given.model.trim();
    if model.is_empty() || given.validate().is_err() {
        return None;
    }
    let maker = given.maker.as_deref().map(str::trim).filter(|m| !m.is_empty());
    db.lenses
        .iter()
        .find(|l| maker.is_none_or(|mk| l.maker.trim().eq_ignore_ascii_case(mk)) && l.models.iter().any(|n| n.trim().eq_ignore_ascii_case(model)))
        .or_else(|| db.find_lens(maker, model, None))
}

/// Look `lens_model` (and its maker, when known) up in the database in use.
pub fn match_lens(lens_make: Option<&str>, lens_model: &str) -> Match {
    match_lens_with(lens_make, lens_model, None)
}

/// [`match_lens`], with a lens given by the caller taking the place of the name match: [`Match::Chosen`] when the
/// database has it, else [`Match::NotFound`] with its name (a chosen lens never falls back to the name match).
pub fn match_lens_with(lens_make: Option<&str>, lens_model: &str, given: Option<&GivenLens>) -> Match {
    let Some(db) = database() else { return Match::NoDatabase };
    if let Some(g) = given {
        return match find_given(&db, g) {
            Some(p) => Match::Chosen(profile_name(p, &g.model)),
            None => Match::NotFound(g.model.trim().chars().take(MAX_NAME).collect()),
        };
    }
    if lens_model.trim().is_empty() {
        return Match::NoLensName;
    }
    match db.find_lens(lens_make, lens_model, None) {
        Some(p) => Match::Found(profile_name(p, lens_model)),
        None => Match::NotFound(lens_model.trim().to_string()),
    }
}

/// Serialises the tests (here and elsewhere in the crate) that change the process-wide database.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// The database profile for a photo with metadata `m`, for its `w × h` (unoriented, default-cropped) image,
/// re-expressed after `orientation`. `None` without a database, lens name, focal length or matching profile.
pub fn lookup(db: &LensDb, m: &lcc_meta::Metadata, w: f64, h: f64, orientation: lcc_geom::Orientation) -> Option<EmbeddedLens> {
    lookup_with(db, m, None, w, h, orientation).map(|(l, _)| l)
}

/// [`lookup`] with an optional lens given by the caller ([`GivenLens`]): its profile (and its focal length and
/// aperture, else the file's) when given, else the profile matched by the metadata's lens name. A given lens the
/// database lacks, or one without a focal length, gives no correction (it never falls back to the name match).
pub fn lookup_with(
    db: &LensDb,
    m: &lcc_meta::Metadata,
    given: Option<&GivenLens>,
    w: f64,
    h: f64,
    orientation: lcc_geom::Orientation,
) -> Option<(EmbeddedLens, Applied)> {
    let valid = |f: f64| f.is_finite() && f > 0.0;
    let camera = db.find_camera(m.make.as_deref().unwrap_or_default(), m.model.as_deref().unwrap_or_default());
    let (lens, focal, aperture, applied) = match given {
        Some(g) => {
            let lens = find_given(db, g)?;
            let focal = g.focal.or(m.focal_length).filter(|f| valid(*f))?;
            (lens, focal, g.aperture.or(m.f_number), Applied::Chosen(profile_name(lens, &g.model)))
        }
        None => {
            let focal = m.focal_length.filter(|f| valid(*f))?;
            let model = m.lens_model.as_deref()?;
            let lens = db.find_lens(m.lens_make.as_deref(), model, camera)?;
            (lens, focal, m.f_number, Applied::Found(profile_name(lens, model)))
        }
    };
    let aperture = aperture.filter(|a| valid(*a));
    // the crop factor: the camera's, else what the 35 mm equivalent focal length implies
    let crop = frame_crop_factor(camera.map(|c| c.crop_factor), focal, m.focal_length_35mm);
    let shot = Shot { focal, aperture, distance: None, crop_factor: crop, width: w, height: h };
    let corr = lens.correction(&shot)?;
    Some((lcc_pipeline::optics::reorient_lens(&corr, orientation, w, h), applied))
}

/// The crop factor of the recorded frame, which scales the profile's radii: the body's (`camera`), else what the
/// 35 mm equivalent focal length implies. A body shooting a crop of its sensor (a full-frame camera in APS-C /
/// Super 35 mode, or switched there by an APS-C lens) records a larger 35 mm equivalent than its own crop factor
/// gives; the frame's corners are then nearer the optical axis, so that crop wins. Within 10 % of the body's, the
/// difference is the tag's whole-millimetre rounding and the body's value stands.
fn frame_crop_factor(camera: Option<f64>, focal: f64, focal_35mm: Option<f64>) -> Option<f64> {
    let frame = focal_35mm.map(|f35| f35 / focal).filter(|c| c.is_finite() && (0.1..=20.0).contains(c));
    match (camera, frame) {
        (Some(body), Some(frame)) if frame > body * 1.1 => Some(frame),
        (body, frame) => body.or(frame),
    }
}

/// [`crate::files::embedded_lens`], else the database profile ([`lookup`]) for a raw with metadata `meta`.
pub fn for_raw(raw: &lcc_raw::RawInfo, meta: impl FnOnce() -> lcc_meta::Metadata) -> Option<EmbeddedLens> {
    resolve_raw(raw, meta, None).map(|(l, _)| l)
}

/// The lens correction for a raw and where it comes from, first that applies: the file's embedded correction,
/// the lens `given` by the caller ([`GivenLens`]), the automatic match by the metadata's lens name; else `None`.
pub fn resolve_raw(raw: &lcc_raw::RawInfo, meta: impl FnOnce() -> lcc_meta::Metadata, given: Option<&GivenLens>) -> Option<(EmbeddedLens, Applied)> {
    resolve(crate::files::embedded_lens(raw), || {
        let db = database()?;
        let (w, h) = raw.developed_size();
        lookup_with(&db, &meta(), given, w as f64, h as f64, raw.orientation)
    })
}

/// The order itself: the file's own correction (`file`) wins; else the database's (`database`, which applies a
/// given lens before the name match).
pub fn resolve(file: Option<EmbeddedLens>, database: impl FnOnce() -> Option<(EmbeddedLens, Applied)>) -> Option<(EmbeddedLens, Applied)> {
    match file {
        Some(l) => Some((l, Applied::Embedded)),
        None => database(),
    }
}

/// One lens of [`list`].
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedLens {
    pub maker: String,
    /// The name to pass back as [`GivenLens::model`].
    pub model: String,
    /// Every name the lens goes by.
    pub models: Vec<String>,
    pub mounts: Vec<String>,
    pub crop_factor: f64,
    /// Calibrated focal lengths, mm (`[min, max]`; none when the profile has no calibration points).
    pub focal: Option<[f64; 2]>,
    pub distortion: bool,
    pub vignetting: bool,
    pub tca: bool,
}

/// What [`list`] filters by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListFilter {
    /// A camera mount (ignoring case); lenses that name no mount are kept.
    pub mount: Option<String>,
    /// The camera's crop factor: lenses calibrated on a smaller crop (a larger frame) or the same are kept, since
    /// their profile covers the frame.
    pub crop: Option<f64>,
    /// Words that must all appear in the maker or a name (ignoring case).
    pub query: Option<String>,
}

/// The database's lenses that pass `filter`, sorted by maker and name: the total that match, and at most `limit`
/// of them from `offset`.
pub fn list(db: &LensDb, filter: &ListFilter, offset: usize, limit: usize) -> (usize, Vec<ListedLens>) {
    let words: Vec<String> = filter.query.as_deref().unwrap_or_default().split_whitespace().take(16).map(str::to_lowercase).collect();
    let mount = filter.mount.as_deref().map(str::trim).filter(|m| !m.is_empty());
    let crop = filter.crop.filter(|c| c.is_finite() && *c > 0.0);
    let mut hits: Vec<&lcc_lensdb::Profile> = db
        .lenses
        .iter()
        .filter(|l| mount.is_none_or(|m| l.mounts.is_empty() || l.mounts.iter().any(|x| x.trim().eq_ignore_ascii_case(m))))
        .filter(|l| crop.is_none_or(|c| !l.crop_factor.is_finite() || l.crop_factor <= 0.0 || l.crop_factor <= c * 1.02))
        .filter(|l| {
            let hay = format!("{} {}", l.maker, l.models.join(" ")).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect();
    hits.sort_by(|a, b| {
        let key = |l: &lcc_lensdb::Profile| (l.maker.to_lowercase(), l.models.first().map(|m| m.to_lowercase()).unwrap_or_default());
        key(a).cmp(&key(b))
    });
    let total = hits.len();
    let out = hits
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|l| {
            let focals = l.distortion.iter().map(|(f, _)| *f).chain(l.tca.iter().map(|(f, _)| *f)).chain(l.vignetting.iter().map(|v| v.focal));
            let (lo, hi) = focals.filter(|f| f.is_finite()).fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), f| (lo.min(f), hi.max(f)));
            ListedLens {
                maker: l.maker.clone(),
                model: l.models.first().cloned().unwrap_or_default(),
                models: l.models.clone(),
                mounts: l.mounts.clone(),
                crop_factor: l.crop_factor,
                focal: (lo <= hi).then_some([lo, hi]),
                distortion: !l.distortion.is_empty(),
                vignetting: !l.vignetting.is_empty(),
                tca: !l.tca.is_empty(),
            }
        })
        .collect();
    (total, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_geom::Orientation;

    const DB: &str = r#"<lensdatabase>
      <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1.5</cropfactor></camera>
      <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
        <calibration>
          <distortion model="poly3" focal="18" k1="-0.03" />
          <vignetting model="pa" focal="18" aperture="3.5" distance="1000" k1="-0.4" k2="0" k3="0" />
        </calibration>
      </lens></lensdatabase>"#;

    fn meta() -> lcc_meta::Metadata {
        lcc_meta::Metadata {
            make: Some("Testcam".into()),
            model: Some("TC-1".into()),
            lens_model: Some("18-55mm F3.5-5.6".into()),
            focal_length: Some(18.0),
            f_number: Some(3.5),
            ..Default::default()
        }
    }

    /// Setting the folder, changing its files and clearing it take effect at once, without a restart.
    #[test]
    fn database_is_swapped_at_runtime() {
        let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        if env_dir().is_some() {
            return;
        }
        let before = configured_dir();
        let dir = std::env::temp_dir().join(format!("lens-profiles-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        set_dir(None);
        assert!(database().is_none());
        assert_eq!(match_lens(None, "18-55mm F3.5-5.6"), Match::NoDatabase);
        let g0 = generation();
        set_dir(Some(dir.clone()));
        assert_ne!(generation(), g0);
        // an empty folder is no database
        assert!(database().is_none());
        std::fs::write(dir.join("t.xml"), DB).unwrap();
        // cached until told
        assert!(database().is_none());
        reload();
        assert_eq!(database().unwrap().lenses.len(), 1);
        assert!(matches!(match_lens(Some("Testcam"), "18-55mm F3.5-5.6"), Match::Found(_)));
        assert_eq!(match_lens(None, "70-200mm F4"), Match::NotFound("70-200mm F4".into()));
        assert_eq!(match_lens(None, "  "), Match::NoLensName);
        // malformed XML next to it is skipped, never a panic
        std::fs::write(dir.join("bad.xml"), "<lensdatabase><lens><model>x</lens").unwrap();
        reload();
        assert_eq!(database().unwrap().lenses.len(), 1);
        assert_eq!(source(), Some((dir.clone(), Origin::Folder)));
        set_dir(None);
        assert!(database().is_none() && source().is_none());
        set_dir(before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exif_lens_is_found_and_converted() {
        let db = LensDb::parse(DB).unwrap();
        let lens = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap();
        assert!(lens.warp.is_some() && lens.vignette.is_some());
        let rotated = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Rotate90).unwrap();
        assert_eq!(rotated.warp.unwrap().center, lcc_geom::Point::new(0.5, 0.5));
        // no lens name, no focal length, an unknown lens: nothing
        assert!(lookup(&db, &lcc_meta::Metadata { lens_model: None, ..meta() }, 6000.0, 4000.0, Orientation::Normal).is_none());
        assert!(lookup(&db, &lcc_meta::Metadata { focal_length: None, ..meta() }, 6000.0, 4000.0, Orientation::Normal).is_none());
        let other = lcc_meta::Metadata { lens_model: Some("70-200mm F4".into()), ..meta() };
        assert!(lookup(&db, &other, 6000.0, 4000.0, Orientation::Normal).is_none());
    }

    const DB3: &str = r#"<lensdatabase>
      <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1.5</cropfactor></camera>
      <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
        <calibration><distortion model="poly3" focal="18" k1="-0.03" /></calibration></lens>
      <lens><maker>Oldglass</maker><model>Oldglass 50mm f/1.8</model><mount>M42</mount><cropfactor>1.0</cropfactor>
        <calibration><distortion model="poly3" focal="50" k1="0.02" />
          <vignetting model="pa" focal="50" aperture="1.8" distance="1000" k1="-0.5" k2="0" k3="0" /></calibration></lens>
      <lens><maker>Tiny</maker><model>Tiny 8mm</model><mount>T</mount><cropfactor>5.0</cropfactor>
        <calibration><distortion model="poly3" focal="8" k1="0.01" /></calibration></lens>
      </lensdatabase>"#;

    fn given(model: &str, focal: Option<f64>) -> GivenLens {
        GivenLens { model: model.into(), focal, ..Default::default() }
    }

    /// The file's own correction wins over a chosen lens, a chosen lens over the match by name, and a chosen lens
    /// the database lacks gives nothing (it never falls back to the name match).
    #[test]
    fn corrections_apply_in_order() {
        let db = LensDb::parse(DB3).unwrap();
        let m = lcc_meta::Metadata { lens_model: Some("18-55mm F3.5-5.6".into()), ..meta() };
        let o = Orientation::Normal;
        let (_, a) = lookup_with(&db, &m, None, 600.0, 400.0, o).unwrap();
        assert_eq!(a, Applied::Found("Testcam 18-55mm f/3.5-5.6".into()));
        let chosen = given("Oldglass 50mm f/1.8", Some(50.0));
        let (lens, a) = lookup_with(&db, &m, Some(&chosen), 600.0, 400.0, o).unwrap();
        assert_eq!(a, Applied::Chosen("Oldglass 50mm f/1.8".into()));
        assert!(lens.vignette.is_some(), "the chosen lens's own calibration");
        assert!(lookup_with(&db, &m, Some(&given("Nonesuch 1000mm", Some(50.0))), 600.0, 400.0, o).is_none());
        // a chosen lens takes the file's focal length when it gives none, and needs one from somewhere
        assert!(lookup_with(&db, &m, Some(&given("Oldglass 50mm f/1.8", None)), 600.0, 400.0, o).is_some());
        let no_focal = lcc_meta::Metadata { focal_length: None, lens_model: None, ..meta() };
        assert!(lookup_with(&db, &no_focal, Some(&given("Oldglass 50mm f/1.8", None)), 600.0, 400.0, o).is_none());
        assert!(lookup_with(&db, &no_focal, Some(&chosen), 600.0, 400.0, o).is_some(), "a file naming no lens, focal given");
        // the file's own correction comes first, whatever was chosen
        let own = EmbeddedLens::default();
        let db_lens = || lookup_with(&db, &m, Some(&chosen), 600.0, 400.0, o);
        assert_eq!(resolve(Some(own), db_lens).map(|(_, a)| a), Some(Applied::Embedded));
        assert_eq!(resolve(None, db_lens).map(|(_, a)| a), Some(Applied::Chosen("Oldglass 50mm f/1.8".into())));
    }

    #[test]
    fn hostile_given_lenses_are_refused_or_match_nothing() {
        let db = LensDb::parse(DB3).unwrap();
        for g in [
            given("", Some(50.0)),
            given(&"x".repeat(MAX_NAME + 1), None),
            given("Oldglass 50mm f/1.8", Some(f64::NAN)),
            given("Oldglass 50mm f/1.8", Some(-3.0)),
            given("Oldglass 50mm f/1.8", Some(f64::INFINITY)),
            GivenLens { aperture: Some(0.0), ..given("Oldglass 50mm f/1.8", Some(50.0)) },
        ] {
            assert!(g.validate().is_err(), "{g:?}");
            assert!(find_given(&db, &g).is_none());
            assert!(lookup_with(&db, &meta(), Some(&g), 600.0, 400.0, Orientation::Normal).is_none());
        }
        // degenerate image sizes never panic
        let g = given("Oldglass 50mm f/1.8", Some(50.0));
        for (w, h) in [(0.0, 0.0), (1.0, 0.0), (f64::NAN, 4.0)] {
            let _ = lookup_with(&db, &meta(), Some(&g), w, h, Orientation::Normal);
        }
    }

    #[test]
    fn lenses_are_listed_by_mount_crop_and_words() {
        let db = LensDb::parse(DB3).unwrap();
        let names = |f: ListFilter| list(&db, &f, 0, 100).1.into_iter().map(|l| l.model).collect::<Vec<_>>();
        assert_eq!(names(ListFilter::default()).len(), 3);
        assert_eq!(names(ListFilter { mount: Some("t".into()), ..Default::default() }), ["Testcam 18-55mm f/3.5-5.6", "Tiny 8mm"]);
        // a 1.5× body takes lenses calibrated on 1.5× or a larger frame, not the 5× one
        assert_eq!(names(ListFilter { crop: Some(1.5), ..Default::default() }), ["Oldglass 50mm f/1.8", "Testcam 18-55mm f/3.5-5.6"]);
        assert_eq!(names(ListFilter { query: Some("OLDGLASS 50".into()), ..Default::default() }), ["Oldglass 50mm f/1.8"]);
        let (total, page) = list(&db, &ListFilter::default(), 1, 1);
        assert_eq!((total, page.len()), (3, 1));
        assert_eq!(page[0].model, "Testcam 18-55mm f/3.5-5.6");
        assert_eq!(list(&db, &ListFilter::default(), usize::MAX, usize::MAX).1.len(), 0);
        let old = &list(&db, &ListFilter { mount: Some("M42".into()), ..Default::default() }, 0, 10).1[0];
        assert_eq!((old.focal, old.vignetting, old.tca), (Some([50.0, 50.0]), true, false));
        assert!(names(ListFilter { crop: Some(f64::NAN), mount: Some(String::new()), query: Some("  ".into()) }).len() == 3);
    }

    /// A full-frame body in APS-C / Super 35 mode (an ILCE-1M2 at 70 mm records 105 mm equivalent) frames only the
    /// middle of the image circle: the profile is applied at the frame's crop factor, not the body's, so the crop's
    /// corners don't get the full frame's corner correction.
    #[test]
    fn in_camera_crop_mode_uses_the_frames_crop_factor() {
        let db = LensDb::parse(
            r#"<lensdatabase>
              <camera><maker>Sony</maker><model>ILCE-1M2</model><mount>Sony E</mount><cropfactor>1</cropfactor></camera>
              <lens><maker>Sigma</maker><model>24-70mm F2.8 DG DN | Art 019</model><mount>Sony E</mount><cropfactor>1.0</cropfactor>
                <calibration>
                  <distortion model="ptlens" focal="70" a="0.0019947" b="0.0035263" c="0.0005016" />
                  <vignetting model="pa" focal="70" aperture="2.8" distance="1000" k1="-0.4" k2="0" k3="0" />
                </calibration>
              </lens></lensdatabase>"#,
        )
        .unwrap();
        let m = |f35: Option<f64>| lcc_meta::Metadata {
            make: Some("SONY".into()),
            model: Some("ILCE-1M2".into()),
            lens_model: Some("24-70mm F2.8 DG DN | Art 019".into()),
            focal_length: Some(70.0),
            focal_length_35mm: f35,
            f_number: Some(2.8),
            ..Default::default()
        };
        let at = |crop: f64, w: f64, h: f64| {
            let shot = Shot { focal: 70.0, aperture: Some(2.8), distance: None, crop_factor: Some(crop), width: w, height: h };
            db.lenses[0].correction(&shot).unwrap()
        };
        let aps_c = lookup(&db, &m(Some(105.0)), 5616.0, 3744.0, Orientation::Normal).unwrap();
        assert_eq!(aps_c, at(1.5, 5616.0, 3744.0));
        assert_ne!(aps_c, at(1.0, 5616.0, 3744.0), "the crop is not corrected as a full frame");
        // full frame, a 35 mm equivalent rounded off by a millimetre, or none at all: the body's crop factor
        for f35 in [Some(70.0), Some(71.0), None] {
            assert_eq!(lookup(&db, &m(f35), 8640.0, 5760.0, Orientation::Normal).unwrap(), at(1.0, 8640.0, 5760.0), "{f35:?}");
        }
        assert_eq!(frame_crop_factor(None, 70.0, Some(105.0)), Some(1.5), "no body in the database: the 35 mm equivalent's");
        assert_eq!(frame_crop_factor(Some(1.0), 0.0, Some(105.0)), Some(1.0), "a hostile focal length is ignored");
    }

    /// A synthetic grid recorded through the profile's barrel distortion comes out of the pipeline's geometry
    /// stage with straight lines.
    #[test]
    fn pipeline_straightens_a_distorted_grid() {
        let db = LensDb::parse(DB).unwrap();
        let profile = &db.lenses[0];
        let (w, h) = (600usize, 400usize);
        let lens = lookup(&db, &meta(), w as f64, h as f64, Orientation::Normal).unwrap();
        // record the grid: each sensor pixel sees the scene point whose distorted position it is
        let n = h as f64 / 2.0;
        let undistort = |x: f64, y: f64| {
            let (dx, dy) = ((x - w as f64 / 2.0) / n, (y - h as f64 / 2.0) / n);
            let rd = dx.hypot(dy);
            let (mut lo, mut hi) = (0.0, 3.0);
            for _ in 0..60 {
                let mid = (lo + hi) / 2.0;
                if mid * profile.distortion_ratio(18.0, mid) < rd {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let s = if rd > 0.0 { lo / rd } else { 1.0 };
            (w as f64 / 2.0 + dx * s * n, h as f64 / 2.0 + dy * s * n)
        };
        // horizontal scene lines every 50 px (2 px wide, dark on light)
        let line = |v: f64| if v.rem_euclid(50.0) < 2.0 { 0.02f32 } else { 0.8 };
        let src = lcc_raster::Rgb32f::from_fn(w, h, |x, y| {
            let (_, sy) = undistort(x as f64 + 0.5, y as f64 + 0.5);
            [line(sy); 3]
        });
        // the dark row nearest y = 51 in each column, sub-pixel (centroid of darkness), as rendered
        let line_rows = |img: &lcc_raster::Rgba8| -> Vec<f64> {
            (20..w - 20)
                .step_by(10)
                .map(|x| {
                    let (mut sw, mut swy) = (0.0, 0.0);
                    for y in 35..70 {
                        let d = 255.0 - img.get(x, y)[1] as f64;
                        if d > 60.0 {
                            sw += d;
                            swy += d * (y as f64 + 0.5);
                        }
                    }
                    swy / sw.max(1e-9)
                })
                .collect()
        };
        let spread = |rows: &[f64]| rows.iter().cloned().fold(f64::MIN, f64::max) - rows.iter().cloned().fold(f64::MAX, f64::min);
        let req = lcc_pipeline::RenderRequest::fit(w, h);
        let mut s = lcc_develop::DevelopSettings::default();
        let info = lcc_pipeline::SourceInfo { lens: Some(lens), ..Default::default() };
        let before = spread(&line_rows(&lcc_pipeline::render(&src, &info, &s, &req).image));
        s.optics.lens_profile = true;
        let after = spread(&line_rows(&lcc_pipeline::render(&src, &info, &s, &req).image));
        assert!(before > 2.0, "the recorded line is curved: {before} px");
        // 8-bit output and a 2 px line: the centroid measurement itself is good to about a pixel
        assert!(after < 1.0 && after < before / 8.0, "corrected line bent by {after} px (uncorrected {before})");
    }
}
