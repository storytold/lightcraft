//! Lens profile corrections from a lens database the user supplies ([`lightcraft_lensdb`])
//! or bundled calibrated lens profiles (Canon EF/RF).
//!
//! When a raw carries no lens correction of its own (DNG opcodes, or a camera correction a raw reader converts,
//! [`crate::files::embedded_lens`]), the lens named in its EXIF is looked up in the lens database.
//! The profile is converted to the same [`lightcraft_develop::EmbeddedLens`] form, so "Enable Profile Corrections", its
//! amount sliders, the GPU path and DNG export treat both alike.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use lightcraft_develop::EmbeddedLens;
use lightcraft_lensdb::{LensDb, Shot};

static BUNDLED_CANON_RF: &str = include_str!("../../../assets/lens-profiles/canon-rf.json");
static BUNDLED_CANON_EF: &str = include_str!("../../../assets/lens-profiles/canon-ef.json");

/// The environment variable naming the lens database directory.
pub const ENV: &str = "LIGHTCRAFT_LENSFUN_DIR";

/// A lens calibration profile deserialized from a JSON profile table.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonProfile {
    maker: String,
    model: String,
    mount: Option<String>,
    #[serde(default = "default_crop")]
    crop_factor: f64,
    #[serde(default)]
    distortion: Vec<JsonDistortion>,
    #[serde(default)]
    tca: Vec<JsonTca>,
}

/// Default full-frame crop factor (1.0) when omitted from a JSON lens profile.
fn default_crop() -> f64 {
    1.0
}

/// A PtLens radial distortion calibration entry at a specific focal length (mm).
#[derive(serde::Deserialize)]
struct JsonDistortion {
    focal: f64,
    a: f64,
    b: f64,
    c: f64,
}

/// A linear transverse chromatic aberration calibration entry at a specific focal length (mm).
#[derive(serde::Deserialize)]
struct JsonTca {
    focal: f64,
    vr: f64,
    vb: f64,
}

/// Converts a deserialized [`JsonProfile`] into a [`lightcraft_lensdb::Profile`].
fn to_profile(jp: JsonProfile) -> lightcraft_lensdb::Profile {
    use lightcraft_lensdb::{Distortion, Tca};
    let mut distortion: Vec<(f64, Distortion)> = jp
        .distortion
        .into_iter()
        .map(|d| (d.focal, Distortion::PtLens { a: d.a, b: d.b, c: d.c }))
        .collect();
    distortion.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut tca: Vec<(f64, Tca)> = jp
        .tca
        .into_iter()
        .map(|t| (t.focal, Tca::Linear { kr: t.vr, kb: t.vb }))
        .collect();
    tca.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mounts = jp.mount.into_iter().collect();
    let crop_factor = if jp.crop_factor > 0.0 { jp.crop_factor } else { 1.0 };

    lightcraft_lensdb::Profile {
        maker: jp.maker,
        models: vec![jp.model],
        mounts,
        crop_factor,
        aspect_ratio: 1.5,
        rectilinear: true,
        distortion,
        tca,
        vignetting: Vec::new(),
    }
}

/// Appends the bundled Canon EF/RF lens profiles and any user-configured JSON profile directories into `db`.
fn load_bundled(db: &mut LensDb) {
    if let Ok(rf) = serde_json::from_str::<Vec<JsonProfile>>(BUNDLED_CANON_RF) {
        db.lenses.extend(rf.into_iter().map(to_profile));
    }
    if let Ok(ef) = serde_json::from_str::<Vec<JsonProfile>>(BUNDLED_CANON_EF) {
        db.lenses.extend(ef.into_iter().map(to_profile));
    }
    if let Ok(dir) = std::env::var("LIGHTCRAFT_LENS_PROFILES") {
        load_json_dir(Path::new(&dir), db);
    }
    if let Some(config_dir) = crate::config::config_dir() {
        load_json_dir(&config_dir.join("lens-profiles"), db);
    }
}

/// Loads all `.json` lens profile files from `dir` into `db`.
fn load_json_dir(dir: &Path, db: &mut LensDb) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(profiles) = serde_json::from_slice::<Vec<JsonProfile>>(&bytes) {
                    db.lenses.extend(profiles.into_iter().map(to_profile));
                }
            }
        }
    }
}

/// The configured database, read once per process; `None` when none is configured or it holds no lenses.
pub fn database() -> Option<&'static LensDb> {
    static DB: OnceLock<Option<LensDb>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = LensDb::default();
        load_bundled(&mut db);
        if let Some(dir) = std::env::var_os(ENV) {
            match LensDb::load_dir(&PathBuf::from(dir)) {
                Ok((loaded, errors)) => {
                    for (file, e) in errors {
                        log::warn!("lens database: skipped {file}: {e}");
                    }
                    db.extend(loaded);
                }
                Err(e) => {
                    log::warn!("{e}");
                }
            }
        }
        (!db.lenses.is_empty()).then_some(db)
    })
    .as_ref()
}

/// Whether any profile in the database matches `lens_model`.
pub fn has_lens(lens_model: &str) -> bool {
    let Some(db) = database() else { return false };
    db.find_lens(None, lens_model, None).is_some()
}

/// The matched profile's canonical name if recognized.
pub fn matched_name(lens_model: &str, camera: &str) -> Option<String> {
    let db = database()?;
    let cam = db.find_camera("", camera);
    let profile = db.find_lens(None, lens_model, cam)?;
    profile.models.first().cloned().or_else(|| Some(profile.maker.clone()))
}

/// Resolves an `EmbeddedLens` for a photo, filling in missing distortion or vignette profiles
/// from the lens database if recognized.
pub fn resolve_photo_lens(
    embedded: Option<lightcraft_develop::EmbeddedLens>,
    lens_name: &str,
    camera: &str,
    focal_mm: Option<f32>,
    aperture: Option<f32>,
) -> Option<lightcraft_develop::EmbeddedLens> {
    if lens_name.trim().is_empty() {
        return embedded;
    }
    let db = database();
    let db_lens = db.and_then(|db| {
        let cam = db.find_camera("", camera);
        let profile = db.find_lens(None, lens_name, cam)?;
        let focal = focal_mm.filter(|f| f.is_finite() && *f > 0.0).map(|f| f as f64)?;
        let shot = Shot {
            focal,
            aperture: aperture.map(|a| a as f64),
            distance: None,
            crop_factor: cam.map(|c| c.crop_factor),
            width: 6000.0,
            height: 4000.0,
        };
        profile.correction(&shot)
    });
    match (embedded, db_lens) {
        (Some(mut l), Some(dl)) => {
            if l.vignette.is_none() {
                l.vignette = dl.vignette;
            }
            if l.warp.is_none() {
                l.warp = dl.warp;
            }
            Some(l)
        }
        (Some(l), None) => Some(l),
        (None, Some(dl)) => Some(dl),
        (None, None) => None,
    }
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
    let crop = frame_crop_factor(camera.map(|c| c.crop_factor), focal, m.focal_length_35mm);
    let shot = Shot { focal, aperture: m.f_number, distance: None, crop_factor: crop, width: w, height: h };
    let corr = lens.correction(&shot)?;
    Some(lightcraft_pipeline::optics::reorient_lens(&corr, orientation, w, h))
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

/// [`crate::files::embedded_lens`], augmented with the database profile ([`lookup`]) when the raw
/// lacks distortion or vignette tables of its own (e.g. Canon CR3 files, which embed radial
/// vignette opcodes but rely on calibrated lens profiles for rectilinear distortion).
pub fn for_raw(raw: &lightcraft_raw::RawInfo, meta: impl FnOnce() -> lightcraft_meta::Metadata) -> Option<EmbeddedLens> {
    let embedded = crate::files::embedded_lens(raw);
    if embedded.as_ref().is_some_and(|l| l.warp.is_some() && l.vignette.is_some()) {
        return embedded;
    }
    let db_lens = database().and_then(|db| {
        let (w, h) = raw.developed_size();
        lookup(db, &meta(), w as f64, h as f64, raw.orientation)
    });
    match (embedded, db_lens) {
        (Some(mut l), Some(dl)) => {
            if l.warp.is_none() {
                l.warp = dl.warp;
            }
            if l.vignette.is_none() {
                l.vignette = dl.vignette;
            }
            Some(l)
        }
        (Some(l), None) => Some(l),
        (None, Some(dl)) => Some(dl),
        (None, None) => None,
    }
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

    #[test]
    fn bundled_canon_profiles_are_loaded_and_findable() {
        assert!(has_lens("Canon RF 70-200mm F2.8L IS USM"));
        let name = matched_name("RF 70-200mm F2.8L IS USM", "Canon EOS R5");
        assert!(name.is_some(), "Canon RF 70-200mm profile must be matched: {name:?}");
    }

    #[test]
    fn exif_lens_is_found_and_converted() {
        let db = LensDb::parse(DB).unwrap();
        let lens = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap();
        assert!(lens.warp.is_some() && lens.vignette.is_some());
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
