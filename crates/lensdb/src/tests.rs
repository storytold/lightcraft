use super::*;

/// A synthetic database in the lensfun format (made-up coefficients, not lensfun data).
const DB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<lensdatabase version="2">
  <mount><name>Test Mount</name></mount>
  <camera>
    <maker>Testcam Corporation</maker>
    <model>TC-1</model>
    <mount>Test Mount</mount>
    <cropfactor>1.5</cropfactor>
  </camera>
  <camera>
    <maker>Testcam</maker>
    <model>TC-FF</model>
    <mount>Test Mount</mount>
    <cropfactor>1</cropfactor>
  </camera>
  <lens>
    <maker>Testcam</maker>
    <model>Testcam 24-70mm f/2.8 &amp; Co</model>
    <model lang="de">Testcam 24-70mm f/2.8 Zoom</model>
    <mount>Test Mount</mount>
    <cropfactor>1</cropfactor>
    <calibration>
      <distortion model="ptlens" focal="24" a="0.01" b="-0.04" c="0" />
      <distortion model="poly3" focal="70" k1="0.01" />
      <tca model="linear" focal="24" kr="1.0004" kb="0.9997" />
      <tca model="poly3" focal="70" vr="1.0001" vb="0.9999" cr="0" cb="0" br="0.0001" bb="-0.0001" />
      <vignetting model="pa" focal="24" aperture="2.8" distance="1000" k1="-0.6" k2="0.2" k3="-0.05" />
      <vignetting model="pa" focal="24" aperture="8" distance="1000" k1="-0.2" k2="0" k3="0" />
      <vignetting model="pa" focal="70" aperture="2.8" distance="1000" k1="-0.3" k2="0" k3="0" />
    </calibration>
  </lens>
  <lens>
    <maker>Testcam</maker>
    <model>Testcam 50mm f/1.8</model>
    <mount>Other Mount</mount>
    <calibration>
      <distortion model="poly5" focal="50" k1="-0.02" k2="0.0003" />
    </calibration>
  </lens>
  <lens>
    <maker>Fishco</maker>
    <model>Fishco 8mm f/3.5 Fisheye</model>
    <type>equisolid</type>
    <calibration>
      <distortion model="poly3" focal="8" k1="0.5" />
    </calibration>
  </lens>
  <lens><maker>Broken</maker><model>Broken 1mm</model><calibration><distortion model="poly3" focal="nan" k1="1"/></calibration></lens>
</lensdatabase>"#;

fn db() -> LensDb {
    LensDb::parse(DB).unwrap()
}

fn shot(focal: f64) -> Shot {
    Shot { focal, aperture: Some(2.8), distance: None, crop_factor: Some(1.0), width: 6000.0, height: 4000.0 }
}

/// The DNG `WarpRectilinear` source position (the pipeline's formula, radial part).
fn dng_source(wp: &EmbeddedWarp, ch: usize, x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
    let (cx, cy) = (wp.center.x * w, wp.center.y * h);
    let m = wp.radius * w.max(h);
    let (dx, dy) = ((x - cx) / m, (y - cy) / m);
    let r2 = dx * dx + dy * dy;
    let [k0, k1, k2, k3, ..] = wp.planes[ch];
    let f = k0 + r2 * (k1 + r2 * (k2 + r2 * k3));
    (cx + dx * f * m, cy + dy * f * m)
}

#[test]
fn parses_cameras_lenses_and_skips_invalid_entries() {
    let db = db();
    assert_eq!(db.cameras.len(), 2);
    assert_eq!(db.lenses.len(), 3, "the lens without valid calibration points is dropped");
    let z = &db.lenses[0];
    assert_eq!(z.models[0], "Testcam 24-70mm f/2.8 & Co", "entities decoded");
    assert_eq!(z.distortion.len(), 2);
    assert_eq!(z.tca.len(), 2);
    assert_eq!(z.vignetting.len(), 3);
    assert_eq!(z.aspect_ratio, 1.5);
    assert!(!db.lenses[2].rectilinear);
}

#[test]
fn malformed_documents_are_errors_not_panics() {
    assert!(LensDb::parse("").is_err());
    assert!(LensDb::parse("<lensdatabase><lens><maker>").is_ok_and(|d| d.lenses.is_empty()));
    assert!(LensDb::parse("<other/>").is_err());
    let deep = "<a>".repeat(100);
    assert_eq!(LensDb::parse(&deep), Err(Error::TooLarge));
}

#[test]
fn finds_cameras_by_exif_strings() {
    let db = db();
    assert_eq!(db.find_camera("TESTCAM CORPORATION", "TC-1").unwrap().crop_factor, 1.5);
    assert_eq!(db.find_camera("Testcam", "tc-ff").unwrap().crop_factor, 1.0);
    assert!(db.find_camera("Testcam", "TC-2").is_none());
    assert!(db.find_camera("Other", "TC-1").is_none());
}

#[test]
fn finds_lenses_by_exif_model_with_or_without_the_maker() {
    let db = db();
    let cam = db.find_camera("Testcam", "TC-FF");
    let l = db.find_lens(None, "24-70mm F2.8 Zoom", cam).unwrap();
    assert_eq!(l.maker, "Testcam");
    assert!(db.find_lens(Some("Testcam"), "Testcam 50mm F1.8", cam).is_some());
    assert!(db.find_lens(None, "24-105mm F4", cam).is_none(), "focal lengths must agree");
    assert!(db.find_lens(Some("Otherco"), "24-70mm f/2.8 Zoom", cam).is_none(), "lens maker must agree");
    assert!(db.find_lens(None, "", cam).is_none());
}

#[test]
fn distortion_matches_the_model_within_half_a_pixel_on_a_grid() {
    let db = db();
    let lens = &db.lenses[0];
    let (w, h) = (6000.0, 4000.0);
    for focal in [24.0, 40.0, 70.0] {
        let wp = lens.correction(&shot(focal)).unwrap().warp.unwrap();
        // lensfun normalisation: r = 1 at half the shorter side (full frame, 3:2)
        let n = h / 2.0;
        let mut worst: f64 = 0.0;
        for gy in 0..=20 {
            for gx in 0..=30 {
                let (x, y) = (gx as f64 * w / 30.0, gy as f64 * h / 20.0);
                let (sx, sy) = dng_source(&wp, 1, x, y, w, h);
                let (dx, dy) = ((x - w / 2.0) / n, (y - h / 2.0) / n);
                let r = dx.hypot(dy);
                let k = lens.distortion_ratio(focal, r);
                let (ex, ey) = (w / 2.0 + dx * k * n, h / 2.0 + dy * k * n);
                worst = worst.max((sx - ex).hypot(sy - ey));
            }
        }
        assert!(worst < 0.5, "{focal} mm: {worst} px");
    }
}

#[test]
fn straight_lines_stay_straight_after_correction() {
    // A synthetic barrel-distorted chart: the camera recorded straight scene lines at r_d = r_u·ratio(r_u).
    // Correcting means: for each output point of a straight line, the warp fetches the recorded point, which must
    // be that line's recorded (curved) image. So sample the curved recorded line by inverting the model, then
    // check the warp sends straight output points onto it.
    let db = db();
    let lens = &db.lenses[0];
    let (w, h) = (6000.0, 4000.0);
    let wp = lens.correction(&shot(24.0)).unwrap().warp.unwrap();
    let n = h / 2.0;
    let y_line = 300.0; // a horizontal line near the top edge
    let mut dev: f64 = 0.0;
    for i in 0..=60 {
        let x = i as f64 * w / 60.0;
        let (sx, sy) = dng_source(&wp, 1, x, y_line, w, h);
        // undo the recorded distortion of (sx, sy): find r_u with r_u·ratio(r_u) = r_d (bisection)
        let (dx, dy) = ((sx - w / 2.0) / n, (sy - h / 2.0) / n);
        let rd = dx.hypot(dy);
        let (mut lo, mut hi) = (0.0, 3.0);
        for _ in 0..80 {
            let mid = (lo + hi) / 2.0;
            if mid * lens.distortion_ratio(24.0, mid) < rd {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let s = if rd > 0.0 { lo / rd } else { 1.0 };
        let uy = h / 2.0 + dy * s * n;
        dev = dev.max((uy - y_line).abs());
    }
    assert!(dev < 0.5, "line bent by {dev} px");
}

#[test]
fn tca_scales_red_and_blue_planes() {
    let db = db();
    let wp = db.lenses[0].correction(&shot(24.0)).unwrap().warp.unwrap();
    let (w, h) = (6000.0, 4000.0);
    let (rx, _) = dng_source(&wp, 0, w, h / 2.0, w, h);
    let (gx, _) = dng_source(&wp, 1, w, h / 2.0, w, h);
    let (bx, _) = dng_source(&wp, 2, w, h / 2.0, w, h);
    assert!(((rx - w / 2.0) / (gx - w / 2.0) - 1.0004).abs() < 1e-5, "{rx} {gx}");
    assert!(((bx - w / 2.0) / (gx - w / 2.0) - 0.9997).abs() < 1e-5, "{bx} {gx}");
}

#[test]
fn vignetting_interpolates_in_aperture_and_focal() {
    let db = db();
    let l = &db.lenses[0];
    let at = |f: f64, n: f64| l.vignetting_attenuation(f, Some(n), None, 1.0).unwrap();
    assert!((at(24.0, 2.8) - 0.55).abs() < 1e-12);
    assert!((at(24.0, 8.0) - 0.8).abs() < 1e-12);
    let mid = at(24.0, 2.8 * 2f64.powf((8f64 / 2.8).log2() / 2.0));
    assert!((mid - 0.675).abs() < 1e-9, "{mid}");
    assert!((at(47.0, 2.8) - 0.625).abs() < 1e-9);
    // the gain undoes the falloff at the corner
    let v = l.correction(&shot(24.0)).unwrap().vignette.unwrap();
    let g: f64 = 1.0 + v.k.iter().enumerate().map(|(i, k)| k * 1f64.powi(2 * (i as i32 + 1))).sum::<f64>();
    assert!((g * 0.55 - 1.0).abs() < 0.01, "{g}");
}

#[test]
fn crop_factor_scales_the_profile() {
    // On a 1.5× body the image covers the centre of the full-frame calibration: much less distortion at its corner.
    let db = db();
    let l = &db.lenses[1];
    let ff = l.correction(&shot(50.0)).unwrap().warp.unwrap();
    let aps = l.correction(&Shot { crop_factor: Some(1.5), ..shot(50.0) }).unwrap().warp.unwrap();
    let corner = |wp: &EmbeddedWarp| dng_source(wp, 1, 0.0, 0.0, 6000.0, 4000.0);
    let (a, b) = (corner(&ff), corner(&aps));
    let r = |(x, y): (f64, f64)| (x - 3000.0).hypot(y - 2000.0);
    let rc = 3000f64.hypot(2000.0);
    assert!((r(a) - rc).abs() > (r(b) - rc).abs() * 1.5, "{} {}", r(a) - rc, r(b) - rc);
}

#[test]
fn fisheyes_get_no_rectilinear_warp_and_bad_shots_none() {
    let db = db();
    assert!(db.lenses[2].correction(&shot(8.0)).is_none());
    assert!(db.lenses[0].correction(&Shot { width: f64::NAN, ..shot(24.0) }).is_none());
    assert!(db.lenses[0].correction(&Shot { focal: 0.0, ..shot(24.0) }).is_none());
}

#[test]
fn load_dir_reads_xml_files_and_reports_bad_ones() {
    let dir = std::env::temp_dir().join(format!("lensdb-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.xml"), DB).unwrap();
    std::fs::write(dir.join("b.xml"), "<lensdatabase").unwrap();
    std::fs::write(dir.join("c.txt"), "ignored").unwrap();
    let (db, errs) = LensDb::load_dir(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(db.lenses.len(), 3);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(LensDb::load_dir(Path::new("/nonexistent/lensdb")).is_err());
}

#[test]
fn tokens_split_names_like_exif_writes_them() {
    assert_eq!(tokens("FE 24-105mm F4.0 G OSS"), ["fe", "24", "105", "mm", "f", "4", "g", "oss"]);
    assert_eq!(tokens("f/2.8"), ["f", "2.8"]);
}

#[test]
fn spec_names_read_like_lens_names() {
    assert_eq!(spec_name([24.0, 70.0, 2.8, 2.8]), "24-70mm f/2.8");
    assert_eq!(spec_name([18.0, 55.0, 3.5, 5.6]), "18-55mm f/3.5-5.6");
    assert_eq!(spec_name([85.0, 85.0, 1.8, 1.8]), "85mm f/1.8");
    assert_eq!(spec_name([10.5, 10.5, 2.8, 2.8]), "10.5mm f/2.8");
}

#[test]
fn spec_names_need_an_exact_profile_name() {
    let db = db();
    let lens = db.find_lens(None, "24-70mm f/2.8", None).expect("a partial match");
    // the profile's names carry more words than the specification, so the maker-less name is not confirmed
    assert!(!lens.names_exactly("24-70mm f/2.8"));
    assert!(lens.names_exactly("testcam 24-70MM F/2.8 zoom"), "case and punctuation don't matter");
    assert!(!lens.names_exactly("24-70mm f/2.8 Zoom"), "the maker's words count: another maker's lens isn't this one");
    let mut aliased = lens.clone();
    aliased.models.push("24-70mm f/2.8".into());
    assert!(aliased.names_exactly(&spec_name([24.0, 70.0, 2.8, 2.8])));
    assert!(!aliased.names_exactly("24-105mm f/4"));
}

/// Another maker's lens with the same specification scores as high as the aliased one; coming first in the
/// database it must not hide the profile named for the specification.
#[test]
fn a_named_profile_is_found_behind_a_tie() {
    let lens = |maker: &str, models: &str| {
        format!(
            r#"<lens><maker>{maker}</maker>{models}<mount>Test Mount</mount><cropfactor>1</cropfactor>
              <calibration><distortion model="poly3" focal="18" k1="-0.01" /></calibration></lens>"#
        )
    };
    let xml = format!(
        r#"<lensdatabase version="2">{}{}</lensdatabase>"#,
        lens("Otherco", "<model>Otherco 18-55mm f/3.5-5.6</model>"),
        lens("Testcam", "<model>Testcam 18-55mm f/3.5-5.6 VR</model><model>18-55mm f/3.5-5.6</model>"),
    );
    let db = LensDb::parse(&xml).unwrap();
    let name = spec_name([18.0, 55.0, 3.5, 5.6]);
    assert_eq!(db.find_lens(None, &name, None).unwrap().maker, "Otherco", "a plain lookup keeps the first of a tie");
    assert_eq!(db.find_lens_named(None, &name, None).unwrap().maker, "Testcam");
    assert!(db.find_lens_named(None, "70-200mm f/4", None).is_none());
}
