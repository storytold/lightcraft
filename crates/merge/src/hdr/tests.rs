use super::*;

#[path = "../../tests/support/hdr_f32.rs"]
pub(super) mod baseline;

pub(super) fn frame(image: Rgb32f, ev: f64) -> Frame {
    let mut frame = Frame::from_linear_rec2020(image, Some(ev));
    frame.clip = 1.0;
    frame.raw = true;
    frame
}

fn brackets(w: usize, h: usize, n: usize) -> Vec<Frame> {
    (0..n)
        .map(|i| {
            let ev = 2.0 * (i as f64 - (n / 2) as f64);
            let k = 2f32.powf(ev as f32);
            let image = Rgb32f::from_fn(w, h, |x, y| {
                let xf = x as f32 / w as f32;
                let yf = y as f32 / h as f32;
                let texture = 0.12 * (x as f32 * 0.79).sin() * (y as f32 * 0.61).cos();
                let light = 0.02 + 1.2 * xf * yf + texture.abs();
                let mut p = [light, light * 0.72, light * 0.43];
                if (xf - (0.3 + i as f32 * 0.12)).abs() < 0.04 && (yf - 0.55).abs() < 0.08 {
                    p = [0.003, 0.004, 0.002];
                }
                if (xf - (0.5 + i as f32 * 0.04)).abs() < 0.01 && (yf - 0.3).abs() < 0.02 {
                    p = [6.0, 4.0, 5.0];
                }
                p.map(|v| (v * k).min(1.0))
            });
            frame(image, ev)
        })
        .collect()
}

pub(super) fn old_options(opts: &HdrOptions) -> baseline::HdrOptions {
    let deghost = match opts.deghost {
        Deghost::None => baseline::Deghost::None,
        Deghost::Low => baseline::Deghost::Low,
        Deghost::Medium => baseline::Deghost::Medium,
        Deghost::High => baseline::Deghost::High,
    };
    baseline::HdrOptions { align: opts.align, deghost }
}

fn assert_identical(frames: Vec<Frame>, opts: HdrOptions) {
    let old = baseline::merge_hdr(frames.clone(), &old_options(&opts), &crate::no_progress).unwrap();
    let new = merge_hdr(frames, &opts, &crate::no_progress).unwrap();
    let rgb_bits = |image: &Rgb32f| image.data.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>();
    assert_eq!(rgb_bits(&new.radiance), rgb_bits(&old.radiance), "radiance {opts:?}");
    assert_eq!(
        new.ghost.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        old.ghost.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "overlay {opts:?}"
    );
    assert_eq!((new.radiance.width, new.radiance.height), (old.radiance.width, old.radiance.height));
    assert_eq!((new.ghost.width, new.ghost.height), (old.ghost.width, old.ghost.height));
    assert_eq!(new.reference, old.reference);
    assert_eq!(new.ev.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), old.ev.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    assert_eq!(new.ev_exif, old.ev_exif);
    assert_eq!(new.alignments.len(), old.alignments.len(), "alignment count {opts:?}");
    for (a, b) in new.alignments.iter().zip(&old.alignments) {
        assert_eq!(a.h, b.h);
        assert_eq!((a.model, a.inliers, a.rms.to_bits()), (b.model, b.inliers, b.rms.to_bits()));
    }
    assert_eq!(new.color, old.color);
    assert_eq!(new.raw, old.raw);
    assert_eq!(new.orientation, old.orientation);
    assert_eq!(new.metadata, old.metadata);
    assert_eq!(new.baseline_exposure.to_bits(), old.baseline_exposure.to_bits());
    assert_eq!(new.lens, old.lens);
    assert_eq!(new.wb_multipliers, old.wb_multipliers);
}

#[test]
fn exact_reductions_match_the_frozen_f32_merge_bit_for_bit() {
    for (w, h, n) in [(1, 1, 3), (3, 2, 3), (127, 129, 3), (128, 130, 5), (128, 97, 7), (257, 67, 3)] {
        for deghost in [Deghost::None, Deghost::Low, Deghost::Medium, Deghost::High] {
            for align in [false, true] {
                assert_identical(brackets(w, h, n), HdrOptions { align, deghost });
            }
        }
    }
    for value in [0.0, 1.0] {
        let frames = [0.25f64, 1.0, 4.0].map(|k| frame(Rgb32f::filled(17, 131, [value; 3]), k.log2())).to_vec();
        assert_identical(frames, HdrOptions { align: true, deghost: Deghost::High });
    }
}

#[test]
fn streamed_saturation_keeps_exact_maxima_and_edge_intervals() {
    for (w, h) in [(1, 1), (1, 137), (7, 1), (19, 137)] {
        let image = Rgb32f::from_fn(w, h, |x, y| {
            let value = [0.0, -0.0, -1.0, 0.8, 0.96, 1.0, 1e-8][(x + 3 * y) % 7];
            [value, value * 0.7, value * 1.2]
        });
        let expected = baseline::saturation(&image);
        let frames = [frame(image, 0.0)];
        for first in (0..h).step_by(64) {
            let mut rows = rows::SaturationRows::new(&frames, first).unwrap();
            for y in first..(first + 64).min(h) {
                if y > first {
                    rows.advance(&frames, y).unwrap();
                }
                let [a, b, c] = rows.for_row(0, y).unwrap();
                for (x, ((&a, &b), &c)) in a.iter().zip(b).zip(c).enumerate() {
                    // f32::max may choose either signed zero on a tie, including between
                    // separately compiled reductions on x86. For finite maxima, numeric
                    // equality permits only that sign difference, with no error tolerance.
                    // Saturation feeds smoothstep, never a divisor or a sign test. Final
                    // radiance and DNG comparisons remain bit-exact, including signed zeros.
                    assert_eq!(0.0f32.max(a).max(b).max(c), expected[y * w + x], "{w}x{h} at {x},{y}");
                }
            }
        }
    }
}

#[test]
fn two_frame_merge_matches_the_frozen_f32_merge() {
    for deghost in [Deghost::None, Deghost::Low, Deghost::Medium, Deghost::High] {
        for align in [false, true] {
            assert_identical(brackets(97, 129, 2), HdrOptions { align, deghost });
        }
    }
}

#[test]
fn mixed_orientations_match_the_frozen_f32_merge() {
    use lightcraft_geom::Orientation;
    for (w, h, orientations) in [
        (65, 97, [Orientation::Normal, Orientation::FlipH, Orientation::Rotate180]),
        (65, 65, [Orientation::Normal, Orientation::Rotate90, Orientation::Transpose]),
    ] {
        let mut frames = brackets(w, h, 3);
        for (frame, orientation) in frames.iter_mut().zip(orientations) {
            frame.orientation = orientation;
        }
        for deghost in [Deghost::None, Deghost::High] {
            for align in [false, true] {
                assert_identical(frames.clone(), HdrOptions { align, deghost });
            }
        }
    }
}

#[test]
fn cancellation_before_loading_does_not_request_any_frames() {
    let requested = std::cell::Cell::new(0);
    let frames = (0..7).map(|i| {
        requested.set(requested.get() + 1);
        Ok(frame(Rgb32f::filled(3, 2, [0.2; 3]), f64::from(i)))
    });
    let result = merge_hdr_iter(frames, &HdrOptions::default(), &|_, _| false);
    assert_eq!(result.err(), Some(MergeError::Cancelled));
    assert_eq!(requested.get(), 0, "cancellation must precede the first lazy decode");
}

#[test]
fn cancellation_during_loading_does_not_request_later_frames() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for stop_after in [1, 3, 6] {
        let requested = AtomicUsize::new(0);
        let frames = (0..7).map(|i| {
            requested.fetch_add(1, Ordering::Relaxed);
            Ok(frame(Rgb32f::filled(3, 2, [0.2; 3]), f64::from(i)))
        });
        let result = merge_hdr_iter(frames, &HdrOptions::default(), &|_, _| requested.load(Ordering::Relaxed) < stop_after);
        assert_eq!(result.err(), Some(MergeError::Cancelled));
        assert_eq!(requested.load(Ordering::Relaxed), stop_after, "a cancelled bracket must not advance its decoder again");
    }
}

#[test]
fn a_decode_error_after_successful_inputs_is_unchanged_and_stops_loading() {
    let requested = std::cell::Cell::new(0);
    let expected = MergeError::Decode("generated failure in the third photo".into());
    let frames = (0..7).map(|i| {
        requested.set(requested.get() + 1);
        if i == 2 { Err(expected.clone()) } else { Ok(frame(Rgb32f::filled(3, 2, [0.2; 3]), f64::from(i))) }
    });
    let result = merge_hdr_iter(frames, &HdrOptions::default(), &crate::no_progress);
    assert_eq!(result.err(), Some(expected));
    assert_eq!(requested.get(), 3, "the decoder must not continue after an input error");
}

#[test]
fn direct_stored_warp_matches_the_original_lanczos_kernel_bit_for_bit() {
    let mut image = brackets(129, 131, 3).remove(1).image;
    // Exercise zero ties in the shared Lanczos neighbour clamp too. These comparisons
    // must retain every output bit, unlike an unobservable saturation intermediate.
    for (i, pixel) in image.data.iter_mut().take(129 * 32).enumerate() {
        *pixel = if i % 2 == 0 { [-0.0, 0.0, -0.0] } else { [0.0, -1e-8, 0.0] };
    }
    for h in [
        lightcraft_geom::Homography::IDENTITY,
        lightcraft_geom::Homography([1.0, 0.0, 2.375, 0.0, 1.0, -1.625, 0.0, 0.0, 1.0]),
        lightcraft_geom::Homography([1.001, 0.002, -0.25, -0.003, 0.999, 0.3, 1e-5, -2e-5, 1.0]),
    ] {
        let (old, valid) = align::warp(&image, &h);
        let mut stored = frame(image.clone(), 0.0);
        let new_valid = input::warp(&mut stored, &h).unwrap();
        assert_eq!(new_valid, valid);
        assert_eq!(
            stored.image.data.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>(),
            old.data.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>()
        );
    }
}

#[test]
fn iterator_storage_moves_pixels_and_packs_crop_capacity_before_loading_the_next() {
    use std::cell::Cell;
    let reads = Cell::new(0);
    let frames = (0..3).map(|i| {
        reads.set(reads.get() + 1);
        let mut source = frame(Rgb32f::filled(7, 5, [0.2; 3]), f64::from(i));
        source.image.data.reserve_exact(5000);
        Ok(source)
    });
    let packed = input::collect(frames, &crate::no_progress).unwrap();
    assert_eq!(reads.get(), 3);
    assert!(packed.iter().all(|f| f.image.data.capacity() == 35));
    let source = frame(Rgb32f::filled(7, 5, [0.2; 3]), 0.0);
    let address = source.image.data.as_ptr();
    let packed = input::collect([Ok(source)], &crate::no_progress).unwrap();
    assert_eq!(packed[0].image.data.as_ptr(), address, "a compact decoded frame moves without a pixel copy");
}

#[test]
fn malformed_and_non_finite_inputs_return_errors_in_both_entry_points() {
    let good = || frame(Rgb32f::filled(3, 2, [0.2; 3]), 0.0);
    let mut invalid = Vec::new();
    for (w, h, len) in [(0, 0, 0), (0, 1, 0), (3, 2, 5), (3, 2, 7), (usize::MAX, 2, 0), (65_001, 1, 0)] {
        let mut f = good();
        f.image = Rgb32f { width: w, height: h, data: vec![[0.2; 3]; len] };
        invalid.push(f);
    }
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut f = good();
        f.image.data[0][1] = value;
        invalid.push(f);
    }
    for clip in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::from_bits(1)] {
        let mut f = good();
        f.clip = clip;
        invalid.push(f);
    }
    for ev in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut f = good();
        f.exposure = Some(ev);
        invalid.push(f);
        let mut f = good();
        f.baseline_exposure = ev;
        invalid.push(f);
    }
    for f in invalid {
        assert!(merge_hdr(vec![good(), f.clone()], &HdrOptions::default(), &crate::no_progress).is_err());
        assert!(merge_hdr_iter([Ok(good()), Ok(f)], &HdrOptions::default(), &crate::no_progress).is_err());
    }
    assert!(matches!(merge_hdr(vec![good()], &HdrOptions::default(), &crate::no_progress), Err(MergeError::TooFew(1, 2))));
    let huge = vec![frame(Rgb32f::filled(3, 2, [0.1; 3]), -1e300), frame(Rgb32f::filled(3, 2, [0.2; 3]), 1e300)];
    for align in [false, true] {
        assert!(merge_hdr(huge.clone(), &HdrOptions { align, deghost: Deghost::High }, &crate::no_progress).is_err());
        let overflow = vec![frame(Rgb32f::filled(3, 2, [f32::MAX; 3]), -20.0), frame(Rgb32f::filled(3, 2, [f32::MAX; 3]), 0.0)];
        assert!(merge_hdr(overflow, &HdrOptions { align, deghost: Deghost::High }, &crate::no_progress).is_err());
    }
    let mismatch = frame(Rgb32f::filled(4, 2, [0.2; 3]), 0.0);
    assert!(merge_hdr(vec![good(), mismatch], &HdrOptions::default(), &crate::no_progress).is_err());
}

#[test]
fn finite_checks_cover_partial_chunks_and_stop_before_the_next_decode() {
    let reads = std::cell::Cell::new(0);
    let inputs = (0..3).map(|i| {
        reads.set(reads.get() + 1);
        let mut input = frame(Rgb32f::filled(257, 65, [0.2; 3]), f64::from(i));
        if i == 1 {
            input.image.data.last_mut().unwrap()[2] = f32::NAN;
        }
        Ok(input)
    });
    let result = merge_hdr_iter(inputs, &HdrOptions::default(), &crate::no_progress);
    assert_eq!(result.err(), Some(input::bad("RGB samples must be finite")));
    assert_eq!(reads.get(), 2);

    let mut dark = frame(Rgb32f::filled(257, 65, [0.2; 3]), -20.0);
    dark.image.data.last_mut().unwrap()[2] = f32::MAX;
    let reference = frame(Rgb32f::filled(257, 65, [0.2; 3]), 0.0);
    let result = merge_hdr(vec![dark, reference], &HdrOptions::default(), &crate::no_progress);
    assert_eq!(result.err(), Some(input::bad("alignment exposure arithmetic overflows f32")));
}

#[test]
fn binary16_must_not_switch_off_the_reference_highlight_guard() {
    // Fewer than 500 samples deliberately selects the known EXIF ratios [0.25, 1, 4].
    let original: Vec<Frame> = [(0.05, -2.0), (0.80001, 0.0), (1.0, 2.0)].map(|(v, ev)| frame(Rgb32f::filled(1, 1, [v; 3]), ev)).to_vec();
    let mut rounded = original.clone();
    for frame in &mut rounded {
        frame.image.map_in_place(|p| p.map(|v| half::f16::from_f32(v).to_f32()));
    }
    // 0x3a66 is exactly 0.7998046875: the nearest binary16 value is below the cutoff.
    assert_eq!(rounded[1].image.data[0][0], half::f16::from_bits(0x3a66).to_f32());
    let opts = HdrOptions { align: false, deghost: Deghost::None };
    let exact = merge_hdr(original, &opts, &crate::no_progress).unwrap();
    let quantised = merge_hdr(rounded, &opts, &crate::no_progress).unwrap();
    let (a, b) = (exact.radiance.data[0][0], quantised.radiance.data[0][0]);
    assert!((a - 0.80001).abs() < 2e-6, "original radiance {a}");
    assert!((b - 0.679834).abs() < 2e-6, "naive binary16 radiance {b}");
    assert!((a - b).abs() / a > 0.15, "this discontinuity must remain covered before choosing storage");
}

#[test]
fn exposure_cutoffs_and_the_500_sample_fallback_use_original_f32_values() {
    let make = |len: usize, green: f32| {
        let mut f = frame(Rgb32f::filled(len, 1, [green; 3]), 0.0);
        f.clip = 1.0;
        f
    };
    for (count, expected) in [(499, false), (500, true), (501, true)] {
        let a = make(count, 0.0100001);
        let b = make(count, 0.04);
        assert_eq!(log_ratio(&a, None, &b, None).is_some(), expected);
        assert_eq!(log_ratio(&a, None, &b, None), baseline::log_ratio(&a, None, &b, None));
    }
    for value in [0.01, f32::from_bits(0.01f32.to_bits() + 1), f32::from_bits(0.8f32.to_bits() - 1), 0.8] {
        let a = make(500, value);
        let b = make(500, 0.04);
        assert_eq!(log_ratio(&a, None, &b, None), baseline::log_ratio(&a, None, &b, None));
    }
    let a = make(500, 0.0100001);
    let mut rounded = a.clone();
    rounded.image.map_in_place(|p| p.map(|v| half::f16::from_f32(v).to_f32()));
    let b = make(500, 0.04);
    assert!(log_ratio(&a, None, &b, None).is_some());
    // A lower cutoff can also round the other way; eligibility and sample count are decisions,
    // not quantities to which a radiance-error floor may be applied.
    assert_eq!(log_ratio(&rounded, None, &b, None), baseline::log_ratio(&rounded, None, &b, None));

    for (cutoff, raw) in [(0.01f32, true), (0.04, false)] {
        let mut a = make(500, f32::from_bits(cutoff.to_bits() - 1));
        a.raw = raw;
        let mut rounded = a.clone();
        rounded.image.map_in_place(|p| p.map(|v| half::f16::from_f32(v).to_f32()));
        assert!(log_ratio(&a, None, &b, None).is_none());
        assert!(log_ratio(&rounded, None, &b, None).is_some(), "binary16 changed the {cutoff} lower cutoff");
    }
    let mut a = make(500, 0.2);
    a.image.data[499][0] = 0.80001;
    let b = make(500, 0.4);
    assert!(log_ratio(&a, None, &b, None).is_none(), "499 eligible samples must fall back");
    a.image.map_in_place(|p| p.map(|v| half::f16::from_f32(v).to_f32()));
    assert!(log_ratio(&a, None, &b, None).is_some(), "rounding admitted the 500th sample and disabled the EXIF fallback");
}

#[test]
fn deghost_prediction_cutoffs_must_survive_storage() {
    for samples in [[0.05, 0.80001, 1.0], [0.00025, 0.00099999, 0.12]] {
        let frames =
            samples.into_iter().enumerate().map(|(i, value)| frame(Rgb32f::filled(32, 24, [value; 3]), 2.0 * (i as f64 - 1.0))).collect::<Vec<_>>();
        let valid = vec![None; 3];
        let k = [0.25, 1.0, 4.0];
        let (exact, fac) = deghost_maps(&frames, &valid, &k, &[0, 1, 2], 1, 0.35, 1);
        let (old, old_fac) = baseline::deghost_maps(&frames, &valid, &k, &[0, 1, 2], 1, 0.35, 1);
        assert_eq!(fac, old_fac);
        for (a, b) in exact.iter().zip(old) {
            assert_eq!(a.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), b.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
        }
        let mut rounded = frames;
        for frame in &mut rounded {
            frame.image.map_in_place(|p| p.map(|v| half::f16::from_f32(v).to_f32()));
        }
        let (changed, _) = deghost_maps(&rounded, &valid, &k, &[0, 1, 2], 1, 0.35, 1);
        let worst = exact.iter().zip(changed).flat_map(|(a, b)| a.data.iter().copied().zip(b.data)).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(worst > 0.9, "a quantised prediction cutoff must not be mistaken for small output noise: {samples:?}, {worst}");
    }
}

/// A whole-frame f32 fallback can preserve cutoffs conservatively without side data: retain
/// f32 whenever the envelope of original and rounded source values can straddle a cutoff.
/// Lanczos is clamped to its neighbours, so a global envelope is sufficient but very broad.
/// This diagnostic measures its storage cost before selecting a production representation.
fn whole_frame_cutoff_risk(frame: &Frame) -> bool {
    let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
    for &value in frame.image.data.iter().flatten() {
        let half = half::f16::from_f32(value).to_f32();
        if !half.is_finite() {
            return true;
        }
        low = low.min(value).min(half);
        high = high.max(value).max(half);
    }
    [0.001, 0.01, 0.04, 0.8, 0.96].into_iter().any(|scale| low <= scale * frame.clip && high >= scale * frame.clip)
}

#[test]
fn conservative_whole_frame_cutoff_fallback_has_no_storage_saving_on_brackets() {
    for n in [3, 5, 7] {
        let inputs = brackets(127, 129, n);
        let fallback = inputs.iter().filter(|frame| whole_frame_cutoff_risk(frame)).count();
        eprintln!("HALF_CANDIDATE frames={n} f32_fallbacks={fallback} additional_storage_saving_bytes=0");
        assert_eq!(fallback, n, "a useful half representation needs compact original decision data");
    }
    let input = frame(Rgb32f::filled(1, 1, [0.80001; 3]), 0.0);
    assert!(whole_frame_cutoff_risk(&input), "the reviewer's reference must keep its original value");
}

#[test]
fn source_samples_far_from_a_cutoff_can_cross_it_after_a_fractional_warp() {
    let original = Rgb32f::from_fn(8, 8, |x, _| [if x < 4 { 0.7004006 } else { 0.89962 }; 3]);
    assert!(original.data.iter().flatten().all(|v| (*v - 0.8).abs() > 0.09));
    let mut rounded = original.clone();
    rounded.map_in_place(|p| p.map(|v| half::f16::from_f32(v).to_f32()));
    let (a, b) = (original.sample_lanczos3(4.0, 4.0)[0], rounded.sample_lanczos3(4.0, 4.0)[0]);
    assert!(a >= 0.8 && b < 0.8, "source-only cutoff exceptions cannot preserve the warped decision: {a} versus {b}");
}
