//! The grain model follows the photo's process (`docs/process-versions.md`): V1 and V2 keep the
//! value-noise grain they always rendered with; the particle grain comes with V3.
//!
//! The legacy check is AlexChambers' reproduction from the review of PR #1060: it renders a gray
//! frame through the public CPU API and compares it with the value-noise formula as it stood before
//! the particle grain landed.

use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
use lightcraft_develop::{DevelopSettings, Process, ProcessVersion};
use lightcraft_pipeline::finish::GrainModel;
use lightcraft_pipeline::{DeepSamples, OutputDepth, RenderRequest, SourceInfo, render};
use lightcraft_raster::Rgb32f;

/// The value-noise grain formula of processes V1 and V2, written out independently of the
/// renderer.
fn old_noise(x: f32, y: f32, seed: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let h = |i: i32, j: i32| {
        let mut v = (i as u32).wrapping_mul(0x8da6_b343) ^ (j as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
        v ^= v >> 13;
        v = v.wrapping_mul(0x5bd1_e995);
        v ^= v >> 15;
        (v & 0xffff) as f32 / 32768.0 - 1.0
    };
    let (i, j) = (x0 as i32, y0 as i32);
    let (u, v) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let a = h(i, j) + (h(i + 1, j) - h(i, j)) * u;
    let b = h(i, j + 1) + (h(i + 1, j + 1) - h(i, j + 1)) * u;
    a + (b - a) * v
}

/// Largest difference (linear, and display-encoded levels of 255) between rendering `s` on a
/// 128 × 96 gray frame and the value-noise grain formula applied to the grain-free render.
fn difference_from_value_noise(s: &DevelopSettings) -> (f32, f32) {
    let (w, h) = (128usize, 96usize);
    let src = Rgb32f::filled(w, h, [0.18; 3]);
    let info = SourceInfo::default();
    let req = RenderRequest { depth: OutputDepth::F32Linear, ..RenderRequest::fit(w, h) };
    let got = render(&src, &info, s, &req).deep.unwrap();
    let mut plain = s.clone();
    plain.grain.amount = 0.0;
    let before = render(&src, &info, &plain, &req).deep.unwrap();
    let (DeepSamples::F32(got), DeepSamples::F32(before)) = (got.samples, before.samples) else {
        panic!("float output");
    };
    let cell = ((0.0006 + (s.grain.size / 100.0) as f32 * 0.0024) * w as f32).max(0.6);
    let sc = w as f32 / cell;
    let rough = (s.grain.roughness / 100.0) as f32;
    let amount = (s.grain.amount / 100.0) as f32 * 0.13;
    let (mut max, mut byte_max) = (0.0f32, 0.0f32);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            let e = [linear_to_srgb(before[i]), linear_to_srgb(before[i + 1]), linear_to_srgb(before[i + 2])];
            let gx = (x as f32 + 0.5) / w as f32;
            let gy = (y as f32 + 0.5) / w as f32;
            let g = old_noise(gx * sc, gy * sc, s.grain.seed) * (1.0 - rough * 0.5)
                + old_noise(gx * sc * 2.3, gy * sc * 2.3, s.grain.seed ^ 0x55) * rough * 0.7;
            let lum = 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
            let k = amount * g * (0.35 + 2.6 * lum * (1.0 - lum));
            for c in 0..3 {
                let want = srgb_to_linear((e[c] + k).clamp(0.0, 1.0));
                max = max.max((want - got[i + c]).abs());
                byte_max = byte_max.max((linear_to_srgb(want) - linear_to_srgb(got[i + c])).abs() * 255.0);
            }
        }
    }
    (max, byte_max)
}

fn grain_json(process: Option<u32>) -> DevelopSettings {
    let mut j = serde_json::json!({ "grain": { "amount": 50.0, "size": 25.0, "roughness": 50.0, "seed": 7 } });
    if let Some(p) = process {
        j["process"] = p.into();
    }
    DevelopSettings::from_json(&j).unwrap()
}

/// Review of PR #1060: settings saved before process versions (V1) render with exactly the grain
/// they always had. Failed with the particle grain on every process (16.5 levels of 255 apart).
#[test]
fn legacy_process_keeps_its_grain() {
    let s = grain_json(None);
    assert_eq!(s.process, ProcessVersion::V1);
    let (max, levels) = difference_from_value_noise(&s);
    eprintln!("V1: max linear difference {max}, max encoded difference {levels} levels");
    assert!(max < 1e-5, "V1 grain changed from the established renderer: {max}");
}

#[test]
fn process_v2_keeps_the_value_noise_grain() {
    let s = grain_json(Some(2));
    assert_eq!(s.process.process(), Process::V2);
    let (max, _) = difference_from_value_noise(&s);
    assert!(max < 1e-5, "V2 grain changed from the established renderer: {max}");
}

/// V3 (and new photos, which get the latest process) render the particle grain.
#[test]
fn process_v3_renders_particle_grain() {
    let s = grain_json(Some(3));
    assert_eq!(s.process.process(), Process::V3);
    assert!(matches!(GrainModel::new(&s, 3000.0), Some(GrainModel::Particles(_))));
    let (_, levels) = difference_from_value_noise(&s);
    assert!(levels > 2.0, "V3 should not render the value-noise grain ({levels} levels apart)");

    for p in [None, Some(2)] {
        assert!(matches!(GrainModel::new(&grain_json(p), 3000.0), Some(GrainModel::Value(_))), "{p:?}");
    }
    let new = DevelopSettings { grain: grain_json(None).grain, ..DevelopSettings::default() };
    assert_eq!(new.process, ProcessVersion::LATEST);
    assert!(matches!(GrainModel::new(&new, 3000.0), Some(GrainModel::Particles(_))));
    // no grain on any process when Amount is 0
    let none = DevelopSettings::default();
    assert_eq!(GrainModel::new(&none, 3000.0), None);
}
