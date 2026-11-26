//! Lens profile corrections from a lens database ([`lightcraft_lensdb`]).
//!
//! When a raw carries no lens correction of its own (DNG opcodes, or a camera correction a raw reader converts,
//! [`crate::files::embedded_lens`]), the lens named in its EXIF is looked up in a lensfun-format database. The
//! database is lensfun's `data/db` (CC BY-SA 3.0): never bundled with the app, it is downloaded at run time into the
//! app's data folder ([`crate::lensfun_download`], which the desktop app offers on its first launch and Settings can start), or it is the
//! directory in `LIGHTCRAFT_LENSFUN_DIR` (which wins). The profile is converted to the same
//! [`lightcraft_develop::EmbeddedLens`] form (marked [`LensSource::Database`]), so "Enable Profile Corrections", its
//! amount sliders, the GPU path and DNG export treat both alike. Without a database nothing changes.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use lightcraft_develop::{EmbeddedLens, LensSource};
use lightcraft_lensdb::{LensDb, Shot};

/// The environment variable naming the lens database directory.
pub const ENV: &str = "LIGHTCRAFT_LENSFUN_DIR";

/// The database folder a host set up ([`use_dir`]), read when `LIGHTCRAFT_LENSFUN_DIR` is not set.
static HOST_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
/// The loaded database and the folder it was read from (`None` inside: nothing usable there).
static LOADED: RwLock<Option<(PathBuf, Option<Arc<LensDb>>)>> = RwLock::new(None);
/// Bumped whenever the database in use changes ([`reload`]): photos looked up before may now find a profile.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Bumped when the database's files were replaced ([`replaced`]): profiles found before may now be different.
static REPLACED: AtomicU64 = AtomicU64::new(0);

/// Use `dir` (the app's downloaded lensfun data, [`crate::lensfun_download::default_dir`]) when
/// `LIGHTCRAFT_LENSFUN_DIR` is not set. Hosts with a file system call it; tests and sessions that don't stay
/// without a database.
pub fn use_dir(dir: Option<PathBuf>) {
    *HOST_DIR.write().unwrap_or_else(PoisonError::into_inner) = dir;
    reload();
}

/// The database's files were replaced (a download put a new database in place): read it again, and photos whose
/// profile came from the old one are looked up again too ([`replacements`]).
pub fn replaced() {
    REPLACED.fetch_add(1, Ordering::Relaxed);
    reload();
}

/// Changes whenever the database's files were replaced ([`replaced`]); selecting a folder ([`use_dir`]) or reading
/// it again doesn't change it, so photos with a profile are not read again at every start.
pub fn replacements() -> u64 {
    REPLACED.load(Ordering::Relaxed)
}

/// The folder the host selected ([`use_dir`]), whatever `LIGHTCRAFT_LENSFUN_DIR` says.
pub fn host_dir() -> Option<PathBuf> {
    HOST_DIR.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Read the database again (after a download or an update): the next lookup sees the new data.
pub fn reload() {
    // under the cache lock, so a load of the old files that finishes meanwhile can't be published ([`database`])
    let mut loaded = LOADED.write().unwrap_or_else(PoisonError::into_inner);
    *loaded = None;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Changes whenever the database in use may have changed.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The folder the database is read from: `LIGHTCRAFT_LENSFUN_DIR`, else the host's ([`use_dir`]).
pub fn dir() -> Option<PathBuf> {
    source().map(|(d, _)| d)
}

/// [`dir`], and whether it is the app's own downloaded folder (read only when whole, [`crate::lensfun_download::complete`])
/// rather than the user's.
fn source() -> Option<(PathBuf, bool)> {
    match std::env::var_os(ENV).filter(|v| !v.is_empty()) {
        Some(d) => Some((PathBuf::from(d), false)),
        None => HOST_DIR.read().unwrap_or_else(PoisonError::into_inner).clone().map(|d| (d, true)),
    }
}

/// The database in use, read once per folder (and again after [`reload`]); `None` when none is configured, the
/// folder is missing or it holds no lenses.
pub fn database() -> Option<Arc<LensDb>> {
    let (dir, downloaded) = source()?;
    if let Some((d, db)) = LOADED.read().unwrap_or_else(PoisonError::into_inner).as_ref()
        && *d == dir
    {
        return db.clone();
    }
    let generation = generation();
    // the app's own folder without its marker is a download that never finished: not read (the start fetches it)
    let db = if downloaded && !crate::lensfun_download::complete(&dir) {
        // info, not a warning: before the first download this is every user's state (logged once per reload)
        log::info!("lens database: no finished download in {} yet; lens profiles are not applied", dir.display());
        None
    } else {
        load(&dir, downloaded)
    };
    let mut loaded = LOADED.write().unwrap_or_else(PoisonError::into_inner);
    // a reload while the files were read: they may be the old ones, so they aren't kept (the next call reads again)
    if GENERATION.load(Ordering::Relaxed) == generation {
        *loaded = Some((dir, db.clone()));
    }
    db
}

fn load(dir: &Path, downloaded: bool) -> Option<Arc<LensDb>> {
    let whose = if downloaded { "downloaded" } else { ENV };
    if !dir.is_dir() {
        log::warn!("lens database: {} ({whose}) is missing or not a folder", dir.display());
        return None;
    }
    match LensDb::load_dir(dir) {
        Ok((db, errors)) => {
            let n_files = std::fs::read_dir(dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xml"))).count()
                })
                .unwrap_or(0);
            log::info!("lens database: {files} files, {profiles} profiles", files = n_files, profiles = db.lenses.len());
            for (file, e) in errors {
                log::warn!("lens database: skipped {file}: {e}");
            }
            let (level, line) = summary(dir, whose, &db);
            log::log!(level, "{line}");
            (!db.lenses.is_empty()).then(|| Arc::new(db))
        }
        Err(e) => {
            log::warn!("{e}");
            None
        }
    }
}

/// The log line for a database read from `dir`: where it is and how many lenses and cameras it has, so a user
/// setting up `LIGHTCRAFT_LENSFUN_DIR` can see whether it was found; a folder without lenses is a warning.
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

/// The database's name for the lens a photo's file calls `lens` (taken with `camera`, the make and model as the
/// library shows them): "Canon EF 70-200mm f/2.8L IS II USM". `None` without a database or a match.
pub fn profile_name(db: &LensDb, camera: &str, lens: &str) -> Option<String> {
    let cam = camera.split_once(' ').and_then(|(make, model)| db.find_camera(make, model).or_else(|| db.find_camera(make, camera)));
    let p = db.find_lens(None, lens, cam)?;
    let model = p.models.first()?;
    Some(if model.to_lowercase().starts_with(&p.maker.to_lowercase()) || p.maker.is_empty() { model.clone() } else { format!("{} {model}", p.maker) })
}

/// The database profile for a photo with metadata `m`, for its `w × h` (unoriented, default-cropped) image,
/// re-expressed after `orientation`. `None` without a database, lens name, focal length or matching profile.
pub fn lookup(db: &LensDb, m: &lightcraft_meta::Metadata, w: f64, h: f64, orientation: lightcraft_geom::Orientation) -> Option<EmbeddedLens> {
    let focal = m.focal_length.filter(|f| f.is_finite() && *f > 0.0)?;
    let camera = db.find_camera(m.make.as_deref().unwrap_or_default(), m.model.as_deref().unwrap_or_default());
    let lens = match m.lens_model.as_deref() {
        Some(model) => db.find_lens(m.lens_make.as_deref(), model, camera)?,
        // Nikon DSLRs record only the lens specification ("24-70mm f/2.8"); a profile must be named exactly that
        // (a user's alias), since the specification fits other makers' lenses too
        None => {
            let name = lightcraft_lensdb::spec_name(m.lens_spec?);
            db.find_lens_named(m.lens_make.as_deref(), &name, camera)?
        }
    };
    let crop = frame_crop_factor(camera.and_then(|c| c.crop_factor), focal, m.focal_length_35mm);
    let shot = Shot { focal, aperture: m.f_number, distance: None, crop_factor: crop, width: w, height: h };
    let corr = lens.correction(&shot)?;
    Some(EmbeddedLens { source: LensSource::Database, ..lightcraft_pipeline::optics::reorient_lens(&corr, orientation, w, h) })
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
pub fn for_raw(raw: &lightcraft_raw::RawInfo, meta: impl FnOnce() -> lightcraft_meta::Metadata) -> Option<EmbeddedLens> {
    crate::files::embedded_lens(raw).or_else(|| {
        let db = database()?;
        let (w, h) = raw.developed_size();
        lookup(&db, &meta(), w as f64, h as f64, raw.orientation)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_geom::Orientation;

    const DB: &str = r#"<lensdatabase>
      <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1.5</cropfactor></camera>
      <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
        <calibration>
          <distortion model="poly3" focal="18" k1="-0.03" />
          <vignetting model="pa" focal="18" aperture="3.5" distance="1000" k1="-0.4" k2="0" k3="0" />
        </calibration>
      </lens></lensdatabase>"#;

    fn meta() -> lightcraft_meta::Metadata {
        lightcraft_meta::Metadata {
            make: Some("Testcam".into()),
            model: Some("TC-1".into()),
            lens_model: Some("18-55mm F3.5-5.6".into()),
            focal_length: Some(18.0),
            f_number: Some(3.5),
            ..Default::default()
        }
    }

    /// The log says where the database was read from and what it holds, and warns when it holds no lenses (#2511).
    #[test]
    fn the_log_names_the_folder_and_what_it_holds() {
        let dir = Path::new("/lensfun/db");
        let (level, line) = summary(dir, ENV, &LensDb::parse(DB).unwrap());
        assert_eq!(level, log::Level::Info);
        assert!(line.contains("lenses 1, cameras 1") && line.contains("/lensfun/db") && line.contains(ENV), "{line}");
        let (level, line) = summary(dir, "downloaded", &LensDb::parse("<lensdatabase></lensdatabase>").unwrap());
        assert_eq!(level, log::Level::Warn);
        assert!(line.contains("no lenses") && line.contains("downloaded") && line.contains("lenses 0, cameras"), "{line}");
    }

    #[test]
    fn exif_lens_is_found_and_converted() {
        let db = LensDb::parse(DB).unwrap();
        let lens = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap();
        assert!(lens.warp.is_some() && lens.vignette.is_some());
        assert_eq!(lens.source, LensSource::Database, "the Optics panel says where it came from");
        assert_eq!(profile_name(&db, "Testcam TC-1", "18-55mm F3.5-5.6").as_deref(), Some("Testcam 18-55mm f/3.5-5.6"));
        assert_eq!(profile_name(&db, "Testcam TC-1", "70-200mm F4"), None);
        let rotated = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Rotate90).unwrap();
        assert_eq!(rotated.warp.unwrap().center, lightcraft_geom::Point::new(0.5, 0.5));
        // no lens name, no focal length, an unknown lens: nothing
        assert!(lookup(&db, &lightcraft_meta::Metadata { lens_model: None, ..meta() }, 6000.0, 4000.0, Orientation::Normal).is_none());
        assert!(lookup(&db, &lightcraft_meta::Metadata { focal_length: None, ..meta() }, 6000.0, 4000.0, Orientation::Normal).is_none());
        let other = lightcraft_meta::Metadata { lens_model: Some("70-200mm F4".into()), ..meta() };
        assert!(lookup(&db, &other, 6000.0, 4000.0, Orientation::Normal).is_none());
    }

    /// A Nikon DSLR raw names no lens, only its specification (maker note `0x0084`): it is looked up by the name
    /// built from it, and only a profile named exactly that is used, never another maker's lens that merely shares
    /// the focal range and aperture.
    #[test]
    fn lens_specification_needs_a_profile_named_by_it() {
        let nikon = lightcraft_meta::Metadata { lens_model: None, lens_spec: Some([18.0, 55.0, 3.5, 5.6]), ..meta() };
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
        let named_other = lightcraft_meta::Metadata { lens_model: Some("70-200mm F4".into()), ..nikon.clone() };
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
        let m = |f35: Option<f64>| lightcraft_meta::Metadata {
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
            EmbeddedLens { source: LensSource::Database, ..db.lenses[0].correction(&shot).unwrap() }
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
            let m = lightcraft_meta::Metadata { focal_length_35mm: Some(17.1), ..meta() };
            let expected = db.lenses[0]
                .correction(&Shot { focal: 18.0, aperture: Some(3.5), distance: None, crop_factor: Some(17.1 / 18.0), width: 6000.0, height: 4000.0 })
                .map(|lens| EmbeddedLens { source: LensSource::Database, ..lens })
                .unwrap();
            assert_eq!(lookup(&db, &m, 6000.0, 4000.0, Orientation::Normal).unwrap(), expected, "{crop}");
            let fallback = db.lenses[0]
                .correction(&Shot { focal: 18.0, aperture: Some(3.5), distance: None, crop_factor: None, width: 6000.0, height: 4000.0 })
                .map(|lens| EmbeddedLens { source: LensSource::Database, ..lens })
                .unwrap();
            assert_eq!(lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap(), fallback, "no equivalent: {crop}");
        }
    }

    /// The database that was found is reported: how many files it holds and how many profiles they carry, so a
    /// directory that was pointed at but holds nothing (or holds profiles that never match a photo) is visible in
    /// the log instead of passing unnoticed. The database is read from the environment and kept for the process,
    /// so the check runs in a child process of this test binary.
    #[test]
    fn lens_database_summary_reports_files_and_profiles() {
        let dir = std::env::temp_dir().join(format!("lc-lens-summary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("one.xml"), DB).unwrap();
        std::fs::write(dir.join("two.xml"), DB.replace("Testcam 18-55mm f/3.5-5.6", "Testcam 24-70mm f/2.8")).unwrap();
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args(["--exact", "lens_profiles::tests::lens_database_summary_child", "--ignored", "--test-threads=1", "--nocapture"])
            .env(ENV, &dir)
            .env("LIGHTCRAFT_LOG", "trace")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "the child process failed: {stderr}");
        let logged: Vec<&str> = stderr.lines().filter(|l| l.starts_with("lightcraft: ")).collect();
        assert!(!logged.is_empty(), "the database was read without a word in the log: {stderr}");
        let summary = logged.iter().find(|l| l.contains("lens database:")).expect("the lens database summary line was not logged");
        let mut counts = summary.split_whitespace().filter_map(|t| t.trim_end_matches(',').parse::<usize>().ok());
        assert_eq!(counts.next(), Some(2), "expected 2 files in the lens database summary: {summary}");
        assert_eq!(counts.next(), Some(2), "expected 2 profiles in the lens database summary: {summary}");
    }

    /// Run by [`lens_database_summary_reports_files_and_profiles`] in a child process: read the database the
    /// environment points at, so its summary reaches the log the parent reads.
    #[test]
    #[ignore = "helper process for lens_database_summary_reports_files_and_profiles"]
    fn lens_database_summary_child() {
        crate::logging::install("lightcraft");
        let _ = database();
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
        let src = lightcraft_raster::Rgb32f::from_fn(w, h, |x, y| {
            let (_, sy) = undistort(x as f64 + 0.5, y as f64 + 0.5);
            [line(sy); 3]
        });
        // the dark row nearest y = 51 in each column, sub-pixel (centroid of darkness), as rendered
        let line_rows = |img: &lightcraft_raster::Rgba8| -> Vec<f64> {
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
        let req = lightcraft_pipeline::RenderRequest::fit(w, h);
        let mut s = lightcraft_develop::DevelopSettings::default();
        let info = lightcraft_pipeline::SourceInfo { lens: Some(lens), ..Default::default() };
        let before = spread(&line_rows(&lightcraft_pipeline::render(&src, &info, &s, &req).image));
        s.optics.lens_profile = true;
        let after = spread(&line_rows(&lightcraft_pipeline::render(&src, &info, &s, &req).image));
        assert!(before > 2.0, "the recorded line is curved: {before} px");
        // 8-bit output and a 2 px line: the centroid measurement itself is good to about a pixel
        assert!(after < 1.0 && after < before / 8.0, "corrected line bent by {after} px (uncorrected {before})");
    }
}
