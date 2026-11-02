//! Film grain through the whole pipeline: which model each process renders with, and the crystal
//! grain's resolution independence (`crate::crystal`).

use lightcraft_develop::{DevelopSettings, Process};
use lightcraft_raster::Rgb32f;

use crate::finish::GrainModel;
use crate::{OutputDepth, RenderRequest, SourceInfo, render};

fn on(process: Process, amount: f64) -> DevelopSettings {
    let mut s = DevelopSettings { process: process.version(), ..Default::default() };
    s.grain.amount = amount;
    s
}

/// Display-encoded luminance (0..255, 16-bit precision) of a render of `src` at `w × h`.
fn luma(src: &Rgb32f, s: &DevelopSettings, w: usize, h: usize) -> Vec<f64> {
    let mut req = RenderRequest::fit(w, h);
    req.depth = OutputDepth::U16;
    let r = render(src, &SourceInfo::default(), s, &req);
    let Some(crate::DeepSamples::U16(v)) = r.deep.map(|d| d.samples) else { panic!("no 16-bit render") };
    v.chunks(3).map(|p| (0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64) / 257.0).collect()
}

fn std(v: &[f64]) -> f64 {
    let m = v.iter().sum::<f64>() / v.len() as f64;
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

#[test]
fn processes_before_v3_keep_the_value_noise() {
    for p in Process::ALL {
        let model = GrainModel::new(&on(p, 40.0), 2000.0, false);
        match p {
            Process::V1 | Process::V2 => assert!(matches!(model, Some(GrainModel::Value(_))), "{p:?}"),
            Process::V3 => assert!(matches!(model, Some(GrainModel::Crystal(_))), "{p:?}"),
        }
        assert!(GrainModel::new(&on(p, 0.0), 2000.0, false).is_none(), "{p:?}: no grain at Amount 0");
    }
    let src = Rgb32f::filled(160, 120, [0.18; 3]);
    let (v2, v3) = (luma(&src, &on(Process::V2, 50.0), 160, 120), luma(&src, &on(Process::V3, 50.0), 160, 120));
    assert_ne!(v2, v3, "V3 renders the crystal grain");
}

/// A preview shows what the export shows, box-downscaled to its size (whole pipeline).
#[test]
fn small_render_matches_the_downscaled_large_render() {
    let src = Rgb32f::filled(300, 200, [0.18; 3]);
    let (w, h, k) = (1200, 800, 4);
    for size in [0.0, 25.0, 100.0] {
        let mut s = on(Process::V3, 60.0);
        s.grain.size = size;
        let base = luma(&src, &on(Process::V3, 0.0), w, h);
        let big: Vec<f64> = luma(&src, &s, w, h).iter().zip(&base).map(|(a, b)| a - b).collect();
        let down: Vec<f64> = (0..(w / k) * (h / k))
            .map(|q| {
                let (x, y) = (q % (w / k) * k, q / (w / k) * k);
                (0..k * k).map(|i| big[(y + i / k) * w + x + i % k]).sum::<f64>() / (k * k) as f64
            })
            .collect();
        let small = luma(&src, &s, w / k, h / k);
        let ratio = std(&small) / std(&down);
        assert!((0.95..1.05).contains(&ratio), "size {size}: preview / export grain {ratio:.3}");
    }
}

/// Degenerate and hostile grain settings render finite pixels under V3.
#[test]
fn hostile_grain_settings_render() {
    let src = Rgb32f::filled(40, 30, [0.5, 0.2, 0.05]);
    for (amount, size, rough) in
        [(f64::NAN, 25.0, 50.0), (1e9, -1e9, f64::INFINITY), (50.0, f64::NAN, f64::NAN), (100.0, 100.0, 100.0), (-5.0, 0.0, 0.0)]
    {
        let mut s = on(Process::V3, amount);
        s.grain.size = size;
        s.grain.roughness = rough;
        s.grain.seed = u32::MAX;
        for (w, h) in [(40, 30), (1, 1), (3, 400)] {
            assert!(luma(&src, &s, w, h).iter().all(|v| v.is_finite()), "{amount}/{size}/{rough} at {w}×{h}");
        }
    }
}
