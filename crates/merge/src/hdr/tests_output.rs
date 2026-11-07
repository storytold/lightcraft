//! End-to-end exactness through DNG scaling and the CPU developer. Regions are fixed and
//! non-empty; relative comparisons never divide by a fixed brightness floor.

use lightcraft_pipeline::{DeepSamples, OutputDepth, RenderRequest, SourceInfo};

use super::tests::{baseline, frame, old_options};
use super::*;

const W: usize = 128;
const H: usize = 96;

fn fixture() -> Vec<Frame> {
    (0..3)
        .map(|i| {
            let ev = 2.0 * (i as f64 - 1.0);
            let gain = 2f32.powf(ev as f32);
            let image = Rgb32f::from_fn(W, H, |x, y| {
                let p = if x < 32 {
                    let v = 5e-7 + 1e-4 * (y * 32 + x) as f32 / (32 * H) as f32;
                    [v * 0.7, v, v * 1.3]
                } else if x < 96 {
                    let v = 0.001 + 5.0 * (x - 32) as f32 / 64.0;
                    [v * 1.3, v, v * 0.43]
                } else if x.abs_diff(100 + 6 * i) <= 2 && y.abs_diff(40) <= 2 {
                    [8.0, 5.0, 3.0]
                } else {
                    [0.003, 0.004, 0.005]
                };
                p.map(|v| (v * gain).min(1.0))
            });
            let mut frame = frame(image, ev);
            frame.baseline_exposure = 0.7;
            frame
        })
        .collect()
}

fn relative_error(reference: f32, actual: f32) -> f64 {
    if reference == 0.0 {
        if actual == 0.0 { 0.0 } else { f64::INFINITY }
    } else {
        (f64::from(actual) - f64::from(reference)).abs() / f64::from(reference).abs()
    }
}

fn regions(reference: &Rgb32f, actual: &Rgb32f) {
    for (name, x0, x1) in [("shadow ramp", 0, 32), ("coloured clipping boundary", 32, 96), ("moving highlight", 96, W)] {
        for channel in 0..3 {
            let mut errors = Vec::new();
            for y in 0..H {
                for x in x0..x1 {
                    let (a, b) = (reference.get(x, y)[channel], actual.get(x, y)[channel]);
                    assert_eq!(a.to_bits(), b.to_bits(), "{name}, channel {channel}, {x},{y}");
                    errors.push(relative_error(a, b));
                }
            }
            assert!(!errors.is_empty());
            errors.sort_by(f64::total_cmp);
            let p99 = errors[(errors.len() - 1) * 99 / 100];
            let max = errors[errors.len() - 1];
            assert!(p99 <= 0.002 && max <= 0.01, "{name}, channel {channel}, p99 {p99}, max {max}");
        }
    }
}

fn decode(bytes: &[u8], source_baseline: f64) -> Rgb32f {
    let raw = lightcraft_raw::decode(bytes).unwrap();
    let mut radiance = raw.develop(lightcraft_raw::Method::Bilinear).unwrap();
    let gain = 2f64.powf(raw.color.baseline_exposure - source_baseline) as f32;
    radiance.map_in_place(|p| p.map(|v| v * gain));
    radiance
}

fn developed(source: &Rgb32f, exposure: f64, temp: f64, tint: f64) -> Rgb32f {
    let mut settings = lightcraft_develop::DevelopSettings::default();
    settings.hdr.enabled = true;
    settings.light.exposure = exposure;
    settings.light.shadows = 75.0;
    settings.wb.mode = lightcraft_develop::WbMode::Custom;
    settings.wb.temp = temp;
    settings.wb.tint = tint;
    let request = RenderRequest { depth: OutputDepth::F32Hdr, ..RenderRequest::fit(W, H) };
    let rendered = lightcraft_pipeline::render(source, &SourceInfo::default(), &settings, &request);
    let DeepSamples::F32(samples) = rendered.deep.unwrap().samples else { panic!("expected float developer output") };
    Rgb32f { width: W, height: H, data: samples.as_chunks::<3>().0.to_vec() }
}

#[test]
fn signed_zero_saturation_keeps_radiance_and_dng_bytes_identical() {
    for pattern in 0..4 {
        let mut frames = fixture();
        for frame in &mut frames {
            for (i, pixel) in frame.image.data.iter_mut().enumerate() {
                match pattern {
                    0 => *pixel = [-0.0; 3],
                    1 => *pixel = if i % 2 == 0 { [-0.0, 0.0, -0.0] } else { [0.0, -0.0, 0.0] },
                    2 if i % W < 32 => {
                        *pixel = match i % 3 {
                            0 => [-0.0, 0.0, -0.0],
                            1 => [0.0, -1e-8, 0.0],
                            _ => [f32::from_bits(1), -f32::from_bits(1), 0.0],
                        };
                    }
                    // A clipped reference highlight sees signed-zero saturation in the
                    // other frames, so the highlight guard's m / k / bound path runs too.
                    3 if i % W >= 96 && pixel.iter().all(|v| v.abs() < 0.01) => *pixel = [-0.0, 0.0, -0.0],
                    _ => {}
                }
            }
        }
        for deghost in [Deghost::None, Deghost::Low, Deghost::Medium, Deghost::High] {
            for align in [false, true] {
                let options = HdrOptions { align, deghost };
                let old = baseline::merge_hdr(frames.clone(), &old_options(&options), &crate::no_progress).unwrap();
                let new = merge_hdr(frames.clone(), &options, &crate::no_progress).unwrap();
                regions(&old.radiance, &new.radiance);
                assert_eq!(
                    old.ghost.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    new.ghost.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
                );
                for samples in [crate::DngSamples::Half, crate::DngSamples::Float] {
                    let encode = |image: &Rgb32f| {
                        crate::write_linear_dng(image, &new.color, new.orientation, &new.metadata, new.baseline_exposure, samples, &new.lens).unwrap()
                    };
                    let (a, b) = (encode(&old.radiance), encode(&new.radiance));
                    assert_eq!(a, b, "signed-zero pattern {pattern}, {options:?}, {samples:?}");
                    regions(&decode(&a, old.baseline_exposure), &decode(&b, new.baseline_exposure));
                }
            }
        }
    }
}

#[test]
fn exact_reductions_keep_dng_restored_radiance_shadow_lifting_and_white_balance() {
    for deghost in [Deghost::None, Deghost::High] {
        let options = HdrOptions { align: false, deghost };
        let frames = fixture();
        let old = baseline::merge_hdr(frames.clone(), &old_options(&options), &crate::no_progress).unwrap();
        let new = merge_hdr(frames, &options, &crate::no_progress).unwrap();
        regions(&old.radiance, &new.radiance);
        let encode = |image: &Rgb32f| {
            crate::write_linear_dng(image, &new.color, new.orientation, &new.metadata, new.baseline_exposure, crate::DngSamples::Half, &new.lens)
                .unwrap()
        };
        let (a, b) = (encode(&old.radiance), encode(&new.radiance));
        assert_eq!(a, b, "the image-maximum scale and second binary16 rounding must match");
        let (old_dng, new_dng) = (decode(&a, old.baseline_exposure), decode(&b, new.baseline_exposure));
        regions(&old_dng, &new_dng);
        assert!(old_dng.get(0, 0).iter().all(|v| *v > 0.0), "the fixed shadow region must not vanish in both paths");
        for (ev, temp, tint) in [(0.0, 6500.0, 0.0), (8.0, 8000.0, 20.0), (6.0, 4200.0, -15.0)] {
            let (a, b) = (developed(&old_dng, ev, temp, tint), developed(&new_dng, ev, temp, tint));
            regions(&a, &b);
        }
        let neutral = developed(&old_dng, 0.0, 6500.0, 0.0);
        let lifted = developed(&old_dng, 8.0, 8000.0, 20.0);
        assert!(lifted.get(16, 48)[1] > 2.0 * neutral.get(16, 48)[1], "the developer must actually lift the shadow region");
        let balanced = developed(&old_dng, 0.0, 8000.0, 20.0);
        let ratio = |image: &Rgb32f| {
            let p = image.get(16, 48);
            p[0] / p[2]
        };
        assert!((ratio(&balanced) - ratio(&neutral)).abs() > 0.001, "the developer must actually change white balance");
    }
}

/// One binary16 subnormal rounding is at most half its 2^-24 spacing. A first rounding
/// propagates through both Lanczos axes (absolute tap sums); a second aligned-pixel rounding
/// adds one more half-spacing. The reference-frame budget then divides by exposure and applies
/// the normalised weight. This is distinct from DNG output's own scale and half quantisation.
fn subnormal_budget(exposure: &[f64], normalised_weights: &[f64], lanczos_l1: f64, second_rounding: bool) -> f64 {
    let half_spacing = 2f64.powi(-25);
    exposure.iter().zip(normalised_weights).map(|(k, w)| w * half_spacing * (lanczos_l1.max(1.0) + f64::from(second_rounding)) / k).sum()
}

#[test]
fn the_precision_gate_cannot_hide_erased_shadows_behind_a_fixed_floor() {
    let k = [0.25, 1.0, 4.0];
    let w = k.map(|v| v / 5.25);
    assert_eq!(relative_error(1e-6, 0.0), 1.0);
    for phase in [0.0, 0.125, 0.5, 0.99] {
        let taps = lightcraft_raster::sample::lanczos3_taps(phase);
        let l1 = taps.iter().map(|v| f64::from(*v).abs()).sum::<f64>().powi(2);
        let budget = subnormal_budget(&k, &w, l1, true);
        assert!(budget > 0.0 && budget < 1e-7);
        assert!(1e-6 > budget + 0.01 * 1e-6, "even accounting for Lanczos and two roundings, erasing 1e-6 must fail");
    }
}
