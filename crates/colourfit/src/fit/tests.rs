use super::*;
use crate::dataset::{Sample, White};
use std::path::PathBuf;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn truth() -> (Model, Vec<[f32; 2]>) {
    let cm1 = Mat3([[0.9, -0.3, -0.05], [-0.5, 1.4, 0.1], [-0.1, 0.3, 0.6]]);
    let cm2 = Mat3([[0.8, -0.25, -0.05], [-0.45, 1.3, 0.15], [-0.08, 0.22, 0.7]]);
    let mut fm = vec![0.70, 0.18, 0.08, 0.28, 0.80, -0.08, 0.02, -0.15, 0.95, 0.66, 0.22, 0.08, 0.26, 0.83, -0.09, 0.03, -0.12, 0.92];
    project_fm(&mut fm);
    let fm = fm_of(&fm);
    let mut t = table::identity();
    for h in 0..table::HUES {
        for s in 0..table::SATS {
            let a = (h as f32 * 4.0).to_radians();
            let sat = s as f32 / (table::SATS - 1) as f32;
            t.data[h * table::SATS + s] = [5.0 * a.sin() * sat.min(0.3) / 0.3, 1.0 + 0.08 * a.cos() * sat, 1.0];
        }
    }
    let tone: Vec<[f32; 2]> = std::iter::once([0.0, 0.0])
        .chain((0..32).map(|i| {
            let x = 2f32.powf(-12.0 + 12.0 * i as f32 / 31.0);
            let y = (x.powf(0.5) / (x.powf(0.5) + 0.3) * 1.3).min(0.999);
            [x, y]
        }))
        .collect();
    (Model { cm: [cm1, cm2], fm: Some(fm), hsm: Some([t.clone(), t]) }, tone)
}

fn synthetic(model: &Model, lut: &pipe::Lut) -> Vec<PhotoSamples> {
    let mut rng = Lcg(7);
    let temps = [2800.0, 3200.0, 3600.0, 4200.0, 4800.0, 5200.0, 5600.0, 6000.0, 6500.0, 7000.0, 7500.0, 3000.0, 5400.0, 6200.0];
    temps
        .iter()
        .enumerate()
        .map(|(i, &t)| {
            let xy = lcc_color::cct::temp_tint_to_xy(t, 0.0);
            let g = g_of_temp(t);
            let cm = Mat3(std::array::from_fn(|r| std::array::from_fn(|c| g * model.cm[0].0[r][c] + (1.0 - g) * model.cm[1].0[r][c])));
            let n = cm.apply(xy.to_xyz());
            let n = n.map(|v| v / n[1]);
            let dec = model.photo(&LookModel::default(), 0.0, n);
            let samples = (0..300)
                .map(|_| {
                    let k = 0.02 + 0.6 * rng.next().powi(2);
                    let r: [f64; 3] = std::array::from_fn(|_| 0.1 + 0.9 * rng.next());
                    let camera: [f32; 3] = std::array::from_fn(|c| (n[c] * r[c] * k) as f32);
                    let xyz = lut.eval(dec.scene(camera));
                    Sample { camera, lightroom_xyz: xyz.map(|v| v as f32), weight: 1.0, pos: [0.5, 0.5] }
                })
                .collect();
            PhotoSamples {
                photo: i,
                look: "Adobe Standard".into(),
                make: "TESTCAM".into(),
                model: "Synthetic 1".into(),
                white: White { neutral: n, lightroom_temp_tint: Some((t, 0.0)), baseline_exposure: 0.0 },
                samples,
                held_out: i % 5 == 0,
                raw_path: PathBuf::new(),
                export_path: PathBuf::new(),
                dng: None,
            }
        })
        .collect()
}

#[test]
fn fit_recovers_a_known_profile() {
    let (model, tone) = truth();
    let lut = lut_for(&tone).unwrap();
    eprintln!("zero-slider render per-pixel: {}", lut.per_pixel);
    let photos = synthetic(&model, &lut);
    let cal = fit(&photos, &FitOptions { contributor: Some("Test".into()), rounds: 3, ..Default::default() }).unwrap();
    cal.validate().unwrap();
    let e = &cal.evidence.looks[0];
    eprintln!("held-out ΔE00 before {:?} after {:?}; white points {}", e.before, e.after, cal.evidence.white_points);
    assert!(e.after.median < 1.5, "{e:?}");
    assert!(e.after.median < e.before.median);
    let json = cal.to_json().unwrap();
    assert!(!json.contains("TESTCAM/") && !json.contains("raw_path"));
}

#[test]
fn fit_hostile_inputs_are_errors() {
    assert!(fit(&[], &FitOptions::default()).is_err());
    let (model, tone) = truth();
    let lut = lut_for(&tone).unwrap();
    let mut photos = synthetic(&model, &lut);
    photos.truncate(3);
    for p in &mut photos {
        p.white.neutral = [f64::NAN, 0.0, -1.0];
        p.white.lightroom_temp_tint = Some((f64::INFINITY, f64::NAN));
        for s in &mut p.samples {
            s.camera = [f32::NAN, 1e30, -5.0];
        }
    }
    assert!(fit(&photos, &FitOptions::default()).is_err());
    let mut mixed = synthetic(&model, &lut);
    mixed[1].model = "Other".into();
    assert!(fit(&mixed, &FitOptions::default()).is_err());
}

/// Issue #1119: the gate keeps a fit that beats the current colour for every file, one that loses to it (on raws,
/// the per-photo JPEG fit) but beats the plain colour for files with matrices of their own only, and refuses the rest.
#[test]
fn gate_verdict_scopes_a_fit_that_loses_to_the_per_photo_fit() {
    let d = |median: f32, p95: f32| DeltaE { median, p95 };
    // the issue's numbers: fitted 1.84 / 6.11 against the CR2s' JPEG fit 1.63 / 4.31; far better than plain colour
    assert_eq!(gate_verdict((d(1.63, 4.31), d(1.84, 6.11)), (d(4.0, 10.0), d(1.9, 6.0))), Verdict::OwnMatrixOnly);
    assert_eq!(gate_verdict((d(3.0, 8.0), d(1.5, 4.0)), (d(4.0, 10.0), d(1.9, 6.0))), Verdict::Beats);
    assert_eq!(gate_verdict((d(1.63, 4.31), d(1.84, 6.11)), (d(1.5, 4.0), d(1.9, 6.0))), Verdict::Fails);
    // both the median and p95 must improve
    assert_eq!(gate_verdict((d(2.0, 4.0), d(1.0, 5.0)), (d(2.0, 4.0), d(1.0, 5.0))), Verdict::Fails);
    // hostile numbers never pass
    assert_eq!(gate_verdict((d(f32::NAN, 4.0), d(1.0, 1.0)), (d(f32::NAN, f32::NAN), d(1.0, 1.0))), Verdict::Fails);
}
