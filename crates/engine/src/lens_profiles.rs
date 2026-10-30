//! Lens profile corrections from a lens database the user supplies ([`lightcraft_lensdb`]).
//!
//! When a raw carries no lens correction of its own (DNG opcodes, or a camera correction a raw reader converts,
//! [`crate::files::embedded_lens`]), the lens named in its EXIF is looked up in a lensfun-format database: the
//! directory in `LIGHTCRAFT_LENSFUN_DIR` (e.g. lensfun's `data/db`, CC-BY-SA, which is never bundled with the app).
//! The profile is converted to the same [`lightcraft_develop::EmbeddedLens`] form, so "Enable Profile Corrections", its
//! amount sliders, the GPU path and DNG export treat both alike. Without the variable nothing changes.
use std::path::PathBuf;
use std::sync::OnceLock;

use lightcraft_develop::EmbeddedLens;
use lightcraft_lensdb::{LensDb, Shot};

/// The environment variable naming the lens database directory.
pub const ENV: &str = "LIGHTCRAFT_LENSFUN_DIR";

/// The configured database, read once per process; `None` when none is configured or it holds no lenses.
pub fn database() -> Option<&'static LensDb> {
    static DB: OnceLock<Option<LensDb>> = OnceLock::new();
    DB.get_or_init(|| {
        for dir in lensfun_candidate_dirs() {
            match LensDb::load_dir(&dir) {
                Ok((db, errors)) => {
                    for (file, e) in errors {
                        log::warn!("lens database: skipped {file}: {e}");
                    }
                    if !db.lenses.is_empty() {
                        return Some(db);
                    }
                }
                Err(e) => {
                    log::warn!("lens database load from {}: {e}", dir.display());
                }
            }
        }
        None
    })
    .as_ref()
}

/// Find external lensfun database candidate directories from the environment, user config, and common system locations.
fn lensfun_candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os(ENV) {
        let p = PathBuf::from(d);
        if p.is_dir() {
            dirs.push(p);
        }
    }
    if let Some(p) = crate::config::config_dir().map(|d| d.join("lensfun")).filter(|p| p.is_dir()) {
        dirs.push(p);
    }
    let candidates = [
        "/usr/share/lensfun/version_1",
        "/usr/share/lensfun/version_2",
        "/var/lib/lensfun-updates/version_1",
        "/var/lib/lensfun-updates/version_2",
        "/usr/local/share/lensfun/version_1",
        "/usr/local/share/lensfun/version_2",
        "/opt/homebrew/share/lensfun/version_1",
        "/opt/homebrew/share/lensfun/version_2",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_dir() {
            dirs.push(p);
        }
    }
    dirs
}

/// The database profile for a photo with metadata `m`, for its `w × h` (unoriented, default-cropped) image,
/// re-expressed after `orientation`. `None` without a database, lens name, focal length or matching profile.
pub fn lookup(db: &LensDb, m: &lightcraft_meta::Metadata, w: f64, h: f64, orientation: lightcraft_geom::Orientation) -> Option<EmbeddedLens> {
    let focal = m.focal_length.filter(|f| f.is_finite() && *f > 0.0)?;
    let fallback_cam = sony_fallback_camera(m.make.as_deref(), m.model.as_deref());
    let camera = db.find_camera(m.make.as_deref().unwrap_or_default(), m.model.as_deref().unwrap_or_default()).or(fallback_cam.as_ref());
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

/// Built-in mount and sensor crop-factor fallback for Sony E-mount bodies (e.g. `ILCE-6400`) when an older
/// lensfun XML database lists the lens but not the body.
fn sony_fallback_camera(make: Option<&str>, model: Option<&str>) -> Option<lightcraft_lensdb::Camera> {
    let model = model?.trim();
    if model.is_empty() {
        return None;
    }
    let is_sony = make.is_some_and(|mk| mk.trim().to_ascii_uppercase().starts_with("SONY"))
        || model.starts_with("ILCE-")
        || model.starts_with("ZV-E")
        || model.starts_with("NEX-");
    if !is_sony {
        return None;
    }
    let upper = model.to_ascii_uppercase();
    let crop_factor = if upper.starts_with("ILCE-6")
        || upper.starts_with("ILCE-5")
        || upper.starts_with("ILCE-3")
        || upper.starts_with("ZV-E10")
        || upper.starts_with("NEX-")
    {
        1.53
    } else if upper.starts_with("ILCE-7") || upper.starts_with("ILCE-9") || upper.starts_with("ILCE-1") || upper.starts_with("ZV-E1") {
        1.0
    } else {
        return None;
    };
    Some(lightcraft_lensdb::Camera { maker: "Sony".into(), model: model.into(), mount: "Sony E".into(), crop_factor })
}

/// Sony E-mount bodies whose in-file 16-entry raw-IFD distortion table (`0x7037`) is used by the engine when no
/// DNG opcode or external lensfun database profile is available.
const SONY_EMBEDDED_FALLBACK_MODELS: &[&str] =
    &["ILCE-6400", "ILCE-6000", "ILCE-6100", "ILCE-6300", "ILCE-6500", "ILCE-6600", "ILCE-6700", "ILCE-7M3", "ILCE-7C", "ZV-E10", "ZV-E10M2"];

/// Construct embedded lens distortion correction for Sony E-mount models using their raw-IFD table.
fn sony_embedded_lens(raw: &lightcraft_raw::RawInfo) -> Option<EmbeddedLens> {
    let model = raw.metadata.model.as_deref()?.trim();
    if !SONY_EMBEDDED_FALLBACK_MODELS.contains(&model) {
        return None;
    }
    let sony = raw.lens_metadata.sony.as_ref()?;
    let op = sony.distortion_opcode()?;
    let mut tmp = raw.clone();
    tmp.opcodes.list3.push(op);
    crate::files::embedded_lens(&tmp)
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

/// [`crate::files::embedded_lens`], else the database profile ([`lookup`]) for a raw with metadata `meta`,
/// else the engine's Sony embedded ARW distortion fallback.
pub fn for_raw(raw: &lightcraft_raw::RawInfo, meta: impl FnOnce() -> lightcraft_meta::Metadata) -> Option<EmbeddedLens> {
    crate::files::embedded_lens(raw)
        .or_else(|| {
            let db = database()?;
            let (w, h) = raw.developed_size();
            lookup(db, &meta(), w as f64, h as f64, raw.orientation)
        })
        .or_else(|| sony_embedded_lens(raw))
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

    #[test]
    fn sony_a6400_uses_fallback_aps_c_camera_and_embedded_arw_distortion() {
        // 1. Even when a lensfun XML has the E-mount lens but omits the ILCE-6400 camera entry,
        //    sony_fallback_camera supplies Sony E mount + 1.53 APS-C crop factor.
        let db = LensDb::parse(
            r#"<lensdatabase>
              <lens><maker>Sony</maker><model>E PZ 16-50mm F3.5-5.6 OSS</model><mount>Sony E</mount><cropfactor>1.53</cropfactor>
                <calibration>
                  <distortion model="poly3" focal="16" k1="-0.045" />
                  <vignetting model="pa" focal="16" aperture="3.5" distance="1000" k1="-0.5" k2="0" k3="0" />
                </calibration>
              </lens></lensdatabase>"#,
        )
        .unwrap();
        let m = lightcraft_meta::Metadata {
            make: Some("SONY".into()),
            model: Some("ILCE-6400".into()),
            lens_model: Some("E PZ 16-50mm F3.5-5.6 OSS".into()),
            focal_length: Some(16.0),
            f_number: Some(3.5),
            ..Default::default()
        };
        let lens = lookup(&db, &m, 6000.0, 4000.0, Orientation::Normal).expect("a6400 lensfun profile");
        assert!(lens.warp.is_some() && lens.vignette.is_some());

        // 2. Without any lensfun database, an ILCE-6400 ARW with an embedded 0x7037 distortion table
        //    resolves its embedded lens warp through `for_raw`.
        use lightcraft_tiff::tags::{self as t, photometric};
        use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
        let (w, h) = (72u32, 48u32);
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![14]));
        raw.set(t::COMPRESSION, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::CFA]));
        raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        raw.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
        raw.set(0x7037, Value::SShort(vec![16, 0, 2, 7, 16, 28, 43, 61, 83, 109, 139, 175, 217, 267, 325, 395, 479]));
        raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![2000u16.to_le_bytes().repeat((w * h) as usize)] });
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("SONY".into()));
        ifd0.set(t::MODEL, Value::Ascii("ILCE-6400".into()));
        ifd0.add_sub_ifd(raw);
        let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();
        let info = lightcraft_raw::probe_info(&bytes).unwrap();
        let fallback = sony_embedded_lens(&info).expect("a6400 embedded ARW distortion fallback");
        assert!(fallback.warp.is_some());
    }
}
