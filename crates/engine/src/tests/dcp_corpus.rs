//! A real camera profile end to end: the Nikon D750 ColorChecker DCP and chart NEFs dieli released
//! under CC0 (github.com/dieli/d750-lightcraft-colorchecker, pinned in `D750_COLORCHECKER`,
//! xtask/src/main.rs; `cargo xtask corpus --download`). Each chart is imported, developed with the
//! imported profile, white balanced on the second-lightest grey patch (as in the reference) and
//! exported as sRGB; the 24 patches are then compared in CIELAB (ΔE2000) with Lightroom Classic
//! 15.6's render of the same NEF with the same profile.
//!
//! The comparison is lenient on purpose: the profile is matrix-only, so the colour model is the same,
//! but Lightroom adds its own default tone curve and rendering, which LightCraft does not copy. The
//! test pins the measured agreement against regressions (see `MEAN_LIMIT`, `MAX_LIMIT`) and prints
//! every patch's ΔE. Missing files are reported as skipped.

use std::path::{Path, PathBuf};

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::Session;
use crate::export::{ExportOptions, export_photo};

/// Upper bounds for the mean and the largest patch ΔE2000 against Lightroom. Measured when this test
/// was written (2026-10-22): daylight/flash mean 3.84, max 7.77 (patch 16, yellow); indoor mean 3.35,
/// max 5.24 (patch 13, blue). The grey row agrees within about 1-3; the colour patches come out less
/// saturated than Lightroom's (its own rendering on top of the profile), which is most of the rest.
const MEAN_LIMIT: f64 = 5.0;
const MAX_LIMIT: f64 = 10.0;

fn set_dir() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
        .join("d750-colorchecker")
}

/// Patch centres (row-major, normalized x, y) in Lightroom's render of each chart, a crop around the
/// ColorChecker. Row 4 is the grey row, white to black.
fn reference_centres(chart: &str) -> Vec<(f64, f64)> {
    let (xs, ys): (&[f64], &[f64]) = if chart.contains("indoor") {
        (&[193.0, 361.0, 527.0, 692.0, 857.0, 1022.0], &[152.0, 318.0, 486.0, 660.0])
    } else {
        (&[197.0, 378.0, 555.0, 731.0, 906.0, 1080.0], &[135.0, 312.0, 490.0, 668.0])
    };
    ys.iter().flat_map(|y| xs.iter().map(move |x| (x / 1200.0, y / 801.0))).collect()
}

/// The same patches in the whole NEF frame (our export, uncropped), read off a 1200 × 802 render.
fn frame_centres(chart: &str) -> Vec<(f64, f64)> {
    let (xs, ys): (&[f64], &[f64]) = if chart.contains("indoor") {
        (&[468.0, 528.0, 587.0, 645.0, 704.0, 765.0], &[527.0, 585.0, 644.0, 705.0])
    } else {
        (&[462.0, 512.0, 560.0, 609.0, 658.0, 706.0], &[381.0, 430.0, 479.0, 529.0])
    };
    ys.iter().flat_map(|y| xs.iter().map(move |x| (x / 1200.0, y / 802.0))).collect()
}

/// Mean CIELAB (D50) of a box of half-width `radius` (a fraction of the image width) around each
/// centre, and the largest standard deviation of L* inside a box (a box on a patch edge shows up there).
fn patch_labs(bytes: &[u8], centres: &[(f64, f64)], radius: f64) -> (Vec<[f64; 3]>, f64) {
    let d = lightcraft_codecs::decode(bytes, Default::default()).unwrap();
    let to_xyz = d.space.to_xyz_d50;
    let (w, h) = (d.image.width, d.image.height);
    let r = (w as f64 * radius).max(1.0) as isize;
    let mut worst = 0.0f64;
    let labs = centres
        .iter()
        .map(|&(x, y)| {
            let (cx, cy) = ((x * w as f64) as isize, (y * h as f64) as isize);
            let (mut acc, mut ls) = ([0.0f64; 3], Vec::new());
            for yy in (cy - r).max(0)..(cy + r).min(h as isize) {
                for xx in (cx - r).max(0)..(cx + r).min(w as isize) {
                    let p = d.image.data[yy as usize * w + xx as usize];
                    let rgb = [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])];
                    for (a, v) in acc.iter_mut().zip(rgb) {
                        *a += v;
                    }
                    ls.push(lab(to_xyz.apply(rgb))[0]);
                }
            }
            let n = ls.len().max(1) as f64;
            let mean_l = ls.iter().sum::<f64>() / n;
            worst = worst.max((ls.iter().map(|l| (l - mean_l).powi(2)).sum::<f64>() / n).sqrt());
            lab(to_xyz.apply(acc.map(|v| v / n)))
        })
        .collect();
    (labs, worst)
}

fn lab(xyz: [f64; 3]) -> [f64; 3] {
    let white = [0.9642, 1.0, 0.8249];
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let [fx, fy, fz] = [f(xyz[0] / white[0]), f(xyz[1] / white[1]), f(xyz[2] / white[2])];
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 (Sharma, Wu and Dalal 2005).
fn de2000(a: [f64; 3], b: [f64; 3]) -> f64 {
    use std::f64::consts::PI;
    let (l1, a1, b1, l2, a2, b2) = (a[0], a[1], a[2], b[0], b[1], b[2]);
    let c_bar = ((a1 * a1 + b1 * b1).sqrt() + (a2 * a2 + b2 * b2).sqrt()) / 2.0;
    let g = 0.5 * (1.0 - (c_bar.powi(7) / (c_bar.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = ((a1p * a1p + b1 * b1).sqrt(), (a2p * a2p + b2 * b2).sqrt());
    let hue = |b: f64, a: f64| if b == 0.0 && a == 0.0 { 0.0 } else { b.atan2(a).rem_euclid(2.0 * PI) };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d.abs() <= PI {
            d
        } else if d > PI {
            d - 2.0 * PI
        } else {
            d + 2.0 * PI
        }
    };
    let dhh = 2.0 * (c1p * c2p).sqrt() * (dh / 2.0).sin();
    let l_bar = (l1 + l2) / 2.0;
    let cp_bar = (c1p + c2p) / 2.0;
    let hp_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= PI {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 2.0 * PI {
        (h1p + h2p + 2.0 * PI) / 2.0
    } else {
        (h1p + h2p - 2.0 * PI) / 2.0
    };
    let t = 1.0 - 0.17 * (hp_bar - PI / 6.0).cos() + 0.24 * (2.0 * hp_bar).cos() + 0.32 * (3.0 * hp_bar + PI / 30.0).cos()
        - 0.20 * (4.0 * hp_bar - 63.0 * PI / 180.0).cos();
    let d_theta = 30.0 * (-(((hp_bar * 180.0 / PI) - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cp_bar.powi(7) / (cp_bar.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cp_bar;
    let sh = 1.0 + 0.015 * cp_bar * t;
    let rt = -(2.0 * d_theta * PI / 180.0).sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dhh / sh).powi(2) + rt * (dc / sc) * (dhh / sh)).sqrt()
}

#[test]
fn de2000_matches_the_published_test_pairs() {
    // Sharma, Wu and Dalal (2005), table 1: pairs 1, 7 and 17
    let cases = [
        ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
        ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
        ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
    ];
    for (a, b, e) in cases {
        assert!((de2000(a, b) - e).abs() < 1e-3, "{a:?} {b:?}: {} vs {e}", de2000(a, b));
    }
}

#[test]
fn the_d750_profile_develops_its_charts_close_to_lightroom() {
    let set = set_dir();
    let dcp = set.join("profiles/20261016_NikonD750_DualIllumination_ColorCheckerPassport.dcp");
    let charts = [("daylight_flash", "raw/chart-daylight_flash.NEF"), ("indoor", "raw/chart-indoor.NEF")];
    let mut needed = vec![dcp.clone()];
    for (name, raw) in charts {
        needed.push(set.join(raw));
        needed.push(set.join(format!("images/{name}-lightroom.jpg")));
    }
    if let Some(missing) = needed.iter().find(|p| !p.exists()) {
        lightcraft_testkit::skip!(Corpus, "{} absent (cargo xtask corpus --download)", missing.display());
        return;
    }
    let lib = std::env::temp_dir().join(format!("lc-dcp-d750-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&lib);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("profile.import", &json!({"paths": [dcp.to_string_lossy()]})).unwrap();
    let dcp_id = r["imported"][0]["id"].as_str().expect("the D750 profile imports").to_string();
    assert!(r["imported"][0]["cameraModel"].as_str().unwrap().contains("D750"), "{r}");
    let o = ExportOptions::from_json(&json!({"format": "png", "longEdge": 1200, "colorSpace": "srgb"}));
    let mut report = String::new();
    let mut means = Vec::new();
    for (name, raw) in charts {
        let r = s.execute("library.import", &json!({"paths": [set.join(raw).to_string_lossy()]})).unwrap();
        let id = PhotoId(r["imported"][0].as_u64().expect("the NEF imports"));
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        s.execute("develop.profile", &json!({"id": dcp_id})).unwrap();
        let centres = frame_centres(name);
        let (reference, spread) =
            patch_labs(&std::fs::read(set.join(format!("images/{name}-lightroom.jpg"))).unwrap(), &reference_centres(name), 0.02);
        assert!(spread < 4.0, "{name}: every reference box lies inside its patch (L* spread {spread:.1})");
        // white balance on the second-lightest grey (patch 20), as the reference was made
        let wb = s.execute("develop.wbPick", &json!({"x": centres[19].0, "y": centres[19].1})).unwrap();
        // exposure: the value whose grey row is closest to the reference's (golden-section search)
        let render = |s: &mut Session, ev: f64| {
            s.execute("develop.set", &json!({"control": "light.exposure", "value": ev})).unwrap();
            let e = export_photo(s, id, &o, 1).expect("the chart develops with the D750 profile");
            patch_labs(&e.bytes, &centres, 0.006)
        };
        let grey_de = |(labs, _): &(Vec<[f64; 3]>, f64)| (18..24).map(|i| de2000(labs[i], reference[i])).sum::<f64>() / 6.0;
        let (mut lo, mut hi) = (-1.5f64, 1.5f64);
        let phi = (5f64.sqrt() - 1.0) / 2.0;
        let (mut m1, mut m2) = (hi - phi * (hi - lo), lo + phi * (hi - lo));
        let (mut f1, mut f2) = (grey_de(&render(&mut s, m1)), grey_de(&render(&mut s, m2)));
        for _ in 0..8 {
            if f1 < f2 {
                (hi, m2, f2) = (m2, m1, f1);
                m1 = hi - phi * (hi - lo);
                f1 = grey_de(&render(&mut s, m1));
            } else {
                (lo, m1, f1) = (m1, m2, f2);
                m2 = lo + phi * (hi - lo);
                f2 = grey_de(&render(&mut s, m2));
            }
        }
        let ev = (lo + hi) / 2.0;
        let (ours, spread) = render(&mut s, ev);
        assert!(spread < 4.0, "{name}: every box lies inside its patch (L* spread {spread:.1})");
        // LIGHTCRAFT_DCP_DUMP=<folder>: keep our renders, to check the patch boxes by eye
        if let Some(dir) = std::env::var_os("LIGHTCRAFT_DCP_DUMP") {
            s.execute("develop.set", &json!({"control": "light.exposure", "value": ev})).unwrap();
            let _ = std::fs::write(Path::new(&dir).join(format!("{name}.png")), export_photo(&mut s, id, &o, 1).unwrap().bytes);
        }
        let des: Vec<f64> = ours.iter().zip(&reference).map(|(a, b)| de2000(*a, *b)).collect();
        let mean = des.iter().sum::<f64>() / des.len() as f64;
        let max = des.iter().cloned().fold(0.0, f64::max);
        report += &format!("{name}: wb {} K / tint {}, exposure {ev:+.2}, mean ΔE2000 {mean:.2}, max {max:.2}\n", wb["temp"], wb["tint"]);
        for (i, d) in des.iter().enumerate() {
            report += &format!(
                "  patch {:2}: ΔE {d:5.2}  ours L*a*b* {:6.1} {:6.1} {:6.1}  Lightroom {:6.1} {:6.1} {:6.1}\n",
                i + 1,
                ours[i][0],
                ours[i][1],
                ours[i][2],
                reference[i][0],
                reference[i][1],
                reference[i][2]
            );
        }
        means.push((name, mean, max));
    }
    eprintln!("{report}");
    let _ = std::fs::remove_dir_all(&lib);
    for (name, mean, max) in means {
        assert!(mean < MEAN_LIMIT && max < MAX_LIMIT, "{name}: mean ΔE2000 {mean:.2}, max {max:.2}\n{report}");
    }
}
