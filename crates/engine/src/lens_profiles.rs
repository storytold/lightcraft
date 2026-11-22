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

/// [`set_dir`] with the database already read from `dir`, so the next lookup doesn't parse it again.
pub fn set_dir_loaded(dir: PathBuf, db: Option<Arc<LensDb>>) {
    let env = env_dir().is_some();
    let mut c = cache();
    c.dir = Some(dir);
    // the environment overrides the folder: its database is loaded on demand as before
    c.loaded = if env { None } else { Some(db) };
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
    load_from(dir, Origin::Folder)
}

/// What the log calls a database source: the environment variable by its full name, or the Settings folder.
fn whose(origin: Origin) -> String {
    match origin {
        Origin::Env => lcc_brand::env_var(ENV),
        Origin::Folder => "folder".to_string(),
    }
}

/// [`load`], logging the folder, where it comes from and what it holds (a missing or empty folder is a warning).
fn load_from(dir: &Path, origin: Origin) -> Option<LensDb> {
    let whose = whose(origin);
    if !dir.is_dir() {
        log::warn!("lens database: {} ({whose}) is missing or not a folder", dir.display());
        return None;
    }
    match LensDb::load_dir(dir) {
        Ok((db, errors)) => {
            for (file, e) in errors {
                log::warn!("lens database: skipped {file}: {e}");
            }
            let (level, line) = summary(dir, &whose, &db);
            log::log!(level, "{line}");
            (!db.lenses.is_empty()).then_some(db)
        }
        Err(e) => {
            log::warn!("lens database: {e}");
            None
        }
    }
}

/// The log line for a database read from `dir`: where it is and how many lenses and cameras it has, so a user
/// setting up the environment override can see whether it was found; a folder without lenses is a warning.
fn summary(dir: &Path, whose: &str, db: &LensDb) -> (log::Level, String) {
    let (lenses, cameras) = (db.lenses.len(), db.cameras.len());
    if lenses == 0 {
        (
            log::Level::Warn,
            format!("lens database: no lenses in {} ({whose}): lenses 0, cameras {cameras}; lens profiles are not applied", dir.display()),
        )
    } else {
        (log::Level::Info, format!("lens database: read {} ({whose}): lenses {lenses}, cameras {cameras}", dir.display()))
    }
}

/// The database in use, loaded on first use after a change; `None` when none is configured or it holds no lenses.
pub fn database() -> Option<Arc<LensDb>> {
    let env = env_dir();
    let mut c = cache();
    if let Some(db) = &c.loaded {
        return db.clone();
    }
    let source = env.map(|d| (d, Origin::Env)).or_else(|| c.dir.clone().map(|d| (d, Origin::Folder)));
    // loaded under the lock: a second caller waits for this load instead of repeating it
    let db = source.and_then(|(dir, origin)| load_from(&dir, origin)).map(Arc::new);
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
    /// A profile exists for the given lens (its name), but neither the caller nor the file supplies a focal
    /// length, so no correction applies.
    NoFocal(String),
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

impl From<&lcc_develop::ChosenLens> for GivenLens {
    fn from(c: &lcc_develop::ChosenLens) -> Self {
        GivenLens { maker: c.make.clone(), model: c.model.clone(), focal: c.focal, aperture: None }
    }
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

/// Look `lens_model` (and its maker, when known) up in the database in use. The photo's camera (EXIF make and
/// model) lets a compact's built-in lens match by the camera's mount, as [`lookup`] does.
pub fn match_lens(lens_make: Option<&str>, lens_model: &str, camera: (&str, &str)) -> Match {
    match_lens_with(lens_make, lens_model, camera, None)
}

/// [`match_lens`], with a lens given by the caller taking the place of the name match: [`Match::Chosen`] when the
/// database has it, else [`Match::NotFound`] with its name (a chosen lens never falls back to the name match).
pub fn match_lens_with(lens_make: Option<&str>, lens_model: &str, camera: (&str, &str), given: Option<&GivenLens>) -> Match {
    let Some(db) = database() else { return Match::NoDatabase };
    if let Some(g) = given {
        return match find_given(&db, g) {
            Some(p) => Match::Chosen(profile_name(p, &g.model)),
            None => Match::NotFound(g.model.trim().chars().take(MAX_NAME).collect()),
        };
    }
    if lens_model.trim().is_empty() {
        // a compact's raw may name no lens; its camera's built-in lens is the one [`lookup`] then takes (issue #3855)
        let built_in = db.find_camera(camera.0, camera.1).and_then(|cam| db.find_built_in_lens("", cam));
        return match built_in {
            Some(p) => Match::Found(profile_name(p, "")),
            None => Match::NoLensName,
        };
    }
    match db.find_lens(lens_make, lens_model, db.find_camera(camera.0, camera.1)) {
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
    lookup_framed(db, m, given, w, h, orientation, None)
}

/// Where a raw's developed picture sits on its camera's full sensor, when that is known and is not the camera's
/// normal image area: a full-frame body's 5:4, 1:1, 16:9 or DX crop. A lens profile describes the lens over the
/// camera's normal frame, so such a picture gets the part of it under its pixels ([`lookup_framed`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageArea {
    /// The stored frame (the active area) on the sensor.
    pub window: lcc_raw::SensorWindow,
    /// The default crop within the active area: the developed picture.
    pub crop: lcc_raw::Rect,
}

impl ImageArea {
    /// The image area of a raw whose stored frame is at `window` (`lcc_raw::sensor_window`). `None` (the profile is
    /// applied to the developed picture as a whole, as before) without a window, for the full sensor, or when the
    /// window doesn't describe this raw's active area, and for non-square pixels (the developed picture is then
    /// stretched, and isn't a plain part of the sensor).
    pub fn of(raw: &lcc_raw::RawInfo, window: Option<lcc_raw::SensorWindow>) -> Option<ImageArea> {
        ImageArea::new(window?, [raw.active_area.width, raw.active_area.height], raw.crop, raw.pixel_aspect)
    }

    /// [`Self::of`] from the parts: the stored frame's `window`, the `active` area's size, the default `crop` within
    /// it (as `RawImage::develop_crop` takes it) and the pixel aspect.
    pub fn new(window: lcc_raw::SensorWindow, active: [usize; 2], crop: lcc_raw::Rect, pixel_aspect: f32) -> Option<ImageArea> {
        let [aw, ah] = active;
        if !window.is_valid() || window.is_full() || window.size != active || pixel_aspect.is_nan() || (pixel_aspect - 1.0).abs() >= 0.01 {
            return None;
        }
        let c = crop.clipped(aw, ah);
        let crop = if c.width == 0 || c.height == 0 { lcc_raw::Rect::new(0, 0, aw, ah) } else { c };
        let area = ImageArea { window, crop };
        area.reference().map(|_| area)
    }

    /// The camera's normal frame the profile is evaluated for: the full sensor with this raw's own default-crop
    /// margins (8256 × 5504 on a Z 7, exactly its 3:2 picture), `[w, h]`.
    fn reference(&self) -> Option<[usize; 2]> {
        let [sw, sh] = self.window.sensor;
        let [aw, ah] = self.window.size;
        let w = sw.checked_sub(aw.checked_sub(self.crop.width)?)?;
        let h = sh.checked_sub(ah.checked_sub(self.crop.height)?)?;
        (w >= 2 && h >= 2).then_some([w, h])
    }
}

/// [`lookup_with`] for a picture at `area` on its sensor ([`ImageArea`]): the profile is evaluated for the camera's
/// normal frame with the body's crop factor (a 35 mm equivalent focal length only rounds what the window says
/// exactly), then re-expressed for this picture ([`lcc_pipeline::optics::reframe_lens`]), so each sensor position
/// gets the correction it gets in a full-frame photo. Without an area, the profile spans the `w × h` picture, as it
/// always has.
pub fn lookup_framed(
    db: &LensDb,
    m: &lcc_meta::Metadata,
    given: Option<&GivenLens>,
    w: f64,
    h: f64,
    orientation: lcc_geom::Orientation,
    area: Option<&ImageArea>,
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
            let (lens, model) = match m.lens_model.as_deref() {
                Some(model) => (db.find_lens(m.lens_make.as_deref(), model, camera)?, model.to_string()),
                // Nikon DSLRs record only the lens specification ("24-70mm f/2.8"); a profile must be named exactly that
                // (a user's alias), since the specification fits other makers' lenses too. A compact's DNG records only
                // the specification of its built-in lens, which its camera's own mount pins down; some compacts record
                // not even that (a PowerShot G7 X CR2), and the mount alone is enough (issue #3855).
                None => {
                    let name = m.lens_spec.map(lcc_lensdb::spec_name).unwrap_or_default();
                    let lens = db.find_lens_named(m.lens_make.as_deref(), &name, camera).or_else(|| db.find_built_in_lens(&name, camera?))?;
                    (lens, name)
                }
            };
            (lens, focal, m.f_number, Applied::Found(profile_name(lens, &model)))
        }
    };
    let aperture = aperture.filter(|a| valid(*a));
    let reference = area.and_then(|a| Some((a, a.reference()?))).filter(|(a, _)| [a.crop.width as f64, a.crop.height as f64] == [w, h]);
    let corr = match reference {
        Some((a, [rw, rh])) => {
            // the camera's normal frame, at the body's own crop factor; then the part of it under this picture,
            // which sits as far into it as the stored frame sits into the sensor (both share the default-crop margins)
            let (rw, rh) = (rw as f64, rh as f64);
            let shot = Shot { focal, aperture, distance: None, crop_factor: camera.and_then(|c| c.crop_factor), width: rw, height: rh };
            let full = lens.correction(&shot)?;
            let origin = [a.window.origin[0] as f64, a.window.origin[1] as f64];
            lcc_pipeline::optics::reframe_lens(&full, [rw, rh], [w, h], origin)
        }
        None => {
            // the crop factor: the camera's, else what the 35 mm equivalent focal length implies
            let crop = frame_crop_factor(camera.and_then(|c| c.crop_factor), focal, m.focal_length_35mm);
            let shot = Shot { focal, aperture, distance: None, crop_factor: crop, width: w, height: h };
            lens.correction(&shot)?
        }
    };
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
pub fn for_raw(raw: &lcc_raw::RawInfo, window: Option<lcc_raw::SensorWindow>, meta: impl FnOnce() -> lcc_meta::Metadata) -> Option<EmbeddedLens> {
    resolve_raw(raw, window, meta, None).map(|(l, _)| l)
}

/// The lens correction for a raw and where it comes from, first that applies: the file's embedded correction,
/// the lens `given` by the caller ([`GivenLens`]), the automatic match by the metadata's lens name; else `None`.
/// `window` is where the raw's stored frame sits on its sensor (`lcc_raw::sensor_window` of the file), for a
/// database profile of a smaller image area ([`ImageArea`]).
pub fn resolve_raw(
    raw: &lcc_raw::RawInfo,
    window: Option<lcc_raw::SensorWindow>,
    meta: impl FnOnce() -> lcc_meta::Metadata,
    given: Option<&GivenLens>,
) -> Option<(EmbeddedLens, Applied)> {
    resolve(crate::files::embedded_lens(raw), || {
        let db = database()?;
        let (w, h) = raw.developed_size();
        lookup_framed(&db, &meta(), given, w as f64, h as f64, raw.orientation, ImageArea::of(raw, window).as_ref())
    })
}

/// The lens correction for a raw when the user may have picked a lens (`picked`, [`lcc_develop::Optics::lens`]): a
/// picked lens's database profile, even over the file's own correction (an explicit choice wins); else
/// [`resolve_raw`] without a given lens. A picked lens the database lacks (or without a focal length) gives none.
pub fn resolve_picked(
    raw: &lcc_raw::RawInfo,
    window: Option<lcc_raw::SensorWindow>,
    meta: impl FnOnce() -> lcc_meta::Metadata,
    picked: Option<&GivenLens>,
) -> Option<(EmbeddedLens, Applied)> {
    match picked {
        Some(g) => {
            let db = database()?;
            let (w, h) = raw.developed_size();
            lookup_framed(&db, &meta(), Some(g), w as f64, h as f64, raw.orientation, ImageArea::of(raw, window).as_ref())
        }
        None => resolve_raw(raw, window, meta, None),
    }
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
    pub crop_factor: Option<f64>,
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
        .filter(|l| crop.is_none_or(|c| l.crop_factor.is_none_or(|f| !f.is_finite() || f <= 0.0 || f <= c * 1.02)))
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

    /// A compact's raw may name no lens at all: a PowerShot G7 X CR2 has neither a lens model nor a lens
    /// specification. The database gives the built-in lens the camera's own mount, and that is enough to find it
    /// (issue #3855). A specification, when the file has one, picks among several profiles on that mount.
    #[test]
    fn a_compact_that_names_no_lens_gets_its_built_in_lens() {
        const COMPACT: &str = r#"<lensdatabase>
          <camera><maker>Testcam</maker><model>Testcam Compact C1</model><mount>testcamC1</mount><cropfactor>2.7</cropfactor></camera>
          <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1.5</cropfactor></camera>
          <lens><maker>Testcam</maker><model>Testcam Compact C1 &amp; compatibles</model><model lang="en">fixed lens</model>
            <mount>testcamC1</mount><cropfactor>2.7</cropfactor>
            <calibration>
              <distortion model="poly3" focal="8.8" k1="-0.03" />
              <distortion model="poly3" focal="36.8" k1="0.004" />
            </calibration>
          </lens>
          <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
            <calibration><distortion model="poly3" focal="18" k1="-0.03" /></calibration>
          </lens></lensdatabase>"#;
        let db = LensDb::parse(COMPACT).unwrap();
        let compact = lcc_meta::Metadata {
            make: Some("Testcam".into()),
            model: Some("Testcam Compact C1".into()),
            lens_model: None,
            lens_spec: None,
            focal_length: Some(8.8),
            f_number: Some(4.0),
            ..Default::default()
        };
        let at = |db: &LensDb, m: &lcc_meta::Metadata| lookup(db, m, 5472.0, 3648.0, Orientation::Normal);
        let lens = at(&db, &compact).expect("the camera's built-in lens");
        assert!(lens.warp.is_some());
        // the same profile when the file records only the lens's specification
        let with_spec = lcc_meta::Metadata { lens_spec: Some([8.8, 36.8, 1.8, 2.8]), ..compact.clone() };
        assert_eq!(at(&db, &with_spec), Some(lens));
        // a body with interchangeable lenses that names none still gets nothing: any of its mount's lenses could be on it
        let body = lcc_meta::Metadata { model: Some("TC-1".into()), focal_length: Some(18.0), ..compact.clone() };
        assert!(at(&db, &body).is_none());
        // nor does a camera the database doesn't list
        let unknown = lcc_meta::Metadata { model: Some("Testcam Compact C9".into()), ..compact.clone() };
        assert!(at(&db, &unknown).is_none());
        // two profiles on the compact's mount: the specification's focal range decides, and without one neither is taken
        let second = r#"<lens><maker>Testcam</maker><model>Testcam Compact C1 with tele converter</model><mount>testcamC1</mount>
            <cropfactor>2.7</cropfactor><calibration><distortion model="poly3" focal="62" k1="0.01" /></calibration></lens></lensdatabase>"#;
        let two = LensDb::parse(&COMPACT.replace("</lensdatabase>", second)).unwrap();
        assert_eq!(two.lenses.len(), 3);
        assert_eq!(at(&two, &with_spec), at(&db, &with_spec));
        assert!(at(&two, &compact).is_none());
    }

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
        assert_eq!(match_lens(None, "18-55mm F3.5-5.6", ("", "")), Match::NoDatabase);
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
        assert!(matches!(match_lens(Some("Testcam"), "18-55mm F3.5-5.6", ("", "")), Match::Found(_)));
        assert_eq!(match_lens(None, "70-200mm F4", ("", "")), Match::NotFound("70-200mm F4".into()));
        assert_eq!(match_lens(None, "  ", ("", "")), Match::NoLensName);
        // malformed XML next to it is skipped, never a panic
        std::fs::write(dir.join("bad.xml"), "<lensdatabase><lens><model>x</lens").unwrap();
        reload();
        assert_eq!(database().unwrap().lenses.len(), 1);
        assert_eq!(source(), Some((dir.clone(), Origin::Folder)));
        // a compact's built-in lens matches by the camera's mount only when the camera is known
        std::fs::write(
            dir.join("compact.xml"),
            r#"<lensdatabase>
<camera><maker>Canon</maker><model>Canon PowerShot S5 IS</model><mount>canonS5</mount><cropfactor>6.0</cropfactor></camera>
<lens><maker>Canon</maker><model>Canon PowerShot S5 IS &amp; compatibles (Standard)</model><mount>canonS5</mount><cropfactor>6.0</cropfactor>
<calibration><distortion model="ptlens" focal="6" a="0.019347" b="-0.049194" c="0"/><distortion model="ptlens" focal="72" a="0" b="0" c="0"/></calibration></lens>
</lensdatabase>"#,
        )
        .unwrap();
        reload();
        let exif = "6.0-72.0 mm f/2.7-3.5";
        assert!(matches!(match_lens(None, exif, ("Canon", "Canon PowerShot S5 IS")), Match::Found(_)));
        // an older catalog entry has only the display label ("Canon PowerShot S5 IS"), with no make
        assert!(matches!(match_lens(None, exif, ("", "Canon PowerShot S5 IS")), Match::Found(_)));
        assert_eq!(match_lens(None, exif, ("", "")), Match::NotFound(exif.into()));
        set_dir(None);
        assert!(database().is_none() && source().is_none());
        set_dir(before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The log says where the database was read from and what it holds, and warns when it holds no lenses.
    #[test]
    fn the_log_names_the_folder_and_what_it_holds() {
        let dir = Path::new("/lensfun/db");
        let (level, line) = summary(dir, "folder", &LensDb::parse(DB).unwrap());
        assert_eq!(level, log::Level::Info);
        assert!(line.contains("lenses 1, cameras 1") && line.contains("/lensfun/db") && line.contains("folder"), "{line}");
        let (level, line) = summary(dir, "folder", &LensDb::parse("<lensdatabase></lensdatabase>").unwrap());
        assert_eq!(level, log::Level::Warn);
        assert!(line.contains("no lenses") && line.contains("lenses 0, cameras"), "{line}");
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

    /// A Nikon DSLR raw names no lens, only its specification (maker note `0x0084`): it is looked up by the name
    /// built from it, and only a profile named exactly that is used, never another maker's lens that merely shares
    /// the focal range and aperture.
    #[test]
    fn lens_specification_needs_a_profile_named_by_it() {
        let nikon = lcc_meta::Metadata { lens_model: None, lens_spec: Some([18.0, 55.0, 3.5, 5.6]), ..meta() };
        let at = |db: &LensDb| lookup(db, &nikon, 6000.0, 4000.0, Orientation::Normal);
        // "Testcam 18-55mm f/3.5-5.6" fits the specification, but so would any maker's 18-55 f/3.5-5.6: refused
        assert!(at(&LensDb::parse(DB).unwrap()).is_none());
        let aliased = LensDb::parse(
            &DB.replace("<model>Testcam 18-55mm f/3.5-5.6</model>", "<model>Testcam 18-55mm f/3.5-5.6</model><model>18-55mm f/3.5-5.6</model>"),
        )
        .unwrap();
        let lens = at(&aliased).unwrap();
        assert!(lens.warp.is_some() && lens.vignette.is_some());
        assert_eq!(Some(lens), lookup(&aliased, &meta(), 6000.0, 4000.0, Orientation::Normal), "same profile as by its name");
        // a lens model, when the file has one, still decides
        let named_other = lcc_meta::Metadata { lens_model: Some("70-200mm F4".into()), ..nikon.clone() };
        assert!(lookup(&aliased, &named_other, 6000.0, 4000.0, Orientation::Normal).is_none());
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

    #[test]
    fn unknown_camera_crop_uses_exif_equivalent_without_full_frame_assumption() {
        for crop in [
            "",
            "<cropfactor/>",
            "<cropfactor>bad</cropfactor>",
            "<cropfactor>NaN</cropfactor>",
            "<cropfactor>inf</cropfactor>",
            "<cropfactor>0.049</cropfactor>",
            "<cropfactor>21</cropfactor>",
        ] {
            let xml = DB.replacen("<cropfactor>1.5</cropfactor>", crop, 1).replace("-0.03", "-0.003").replace("-0.4", "-0.04");
            let db = LensDb::parse(&xml).unwrap();
            // A below-1 crop exposes the old synthetic 1.0 body: the 10% crop-mode policy would keep it.
            let m = lcc_meta::Metadata { focal_length_35mm: Some(17.1), ..meta() };
            let expected = db.lenses[0]
                .correction(&Shot { focal: 18.0, aperture: Some(3.5), distance: None, crop_factor: Some(17.1 / 18.0), width: 6000.0, height: 4000.0 })
                .unwrap();
            assert_eq!(lookup(&db, &m, 6000.0, 4000.0, Orientation::Normal).unwrap(), expected, "{crop}");
            let fallback = db.lenses[0]
                .correction(&Shot { focal: 18.0, aperture: Some(3.5), distance: None, crop_factor: None, width: 6000.0, height: 4000.0 })
                .unwrap();
            assert_eq!(lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap(), fallback, "no equivalent: {crop}");
        }
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

#[cfg(test)]
#[path = "tests_lens_crop.rs"]
mod tests_crop;
