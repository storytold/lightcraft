//! Focus stacking on a synthetic bracket: a procedural macro scene whose depth runs left to right,
//! photographed focused at several depths (depth-dependent blur, focus breathing). The stack must
//! be sharp everywhere, unlike any single frame, whatever the frames' order and brightness.

use lightcraft_geom::{Homography, Orientation, Point};
use lightcraft_merge::focus::{FocusOptions, FocusResult, Load, stack_focus};
use lightcraft_merge::frame::Frame;
use lightcraft_merge::synth::{self, FOCUS_BREATHING};
use lightcraft_merge::{MergeError, no_progress};
use lightcraft_raster::Rgb32f;
use lightcraft_raster::resample::{Filter, fit};

const W: usize = 640;
const H: usize = 420;

/// Mean absolute error against the truth in each tenth of the width (a 20 px margin skipped).
fn band_errors(img: &Rgb32f, truth: &Rgb32f) -> Vec<f32> {
    (0..10)
        .map(|b| {
            let (x0, x1) = ((b * W / 10).max(20), ((b + 1) * W / 10).min(W - 20));
            let (mut s, mut k) = (0.0, 0);
            for y in 20..H - 20 {
                for x in x0..x1 {
                    let (p, t) = (img.get(x, y), truth.get(x, y));
                    s += (0..3).map(|c| (p[c] - t[c]).abs()).sum::<f32>();
                    k += 1;
                }
            }
            s / k as f32
        })
        .collect()
}

fn worst(v: &[f32]) -> f32 {
    v.iter().copied().fold(0.0, f32::max)
}

/// `img` (magnified by `from`) as seen magnified by `to`.
fn rezoom(img: &Rgb32f, from: f32, to: f32) -> Rgb32f {
    let (cx, cy) = (W as f32 / 2.0, H as f32 / 2.0);
    Rgb32f::from_fn(W, H, |x, y| img.sample_bilinear(cx + (x as f32 + 0.5 - cx) * from / to, cy + (y as f32 + 0.5 - cy) * from / to))
}

/// Alignment error allowed at the frame's corners (px). Measured 0.4–1.35: features move as the
/// blur, strong here (up to 10 px) and changing across the frame, differs between neighbours.
const MAX_CORNER_ERROR: f64 = 1.5;

fn zoom(k: usize) -> f32 {
    1.0 + FOCUS_BREATHING * k as f32
}

/// Stack `frames` (with `exposures` as their EXIF); `half`: the loader's alignment versions are half size.
fn stack(frames: &[Rgb32f], exposures: &[Option<f64>], half: bool, opts: &FocusOptions) -> Result<FocusResult, MergeError> {
    let load = |i: usize, what: Load| -> Result<Frame, MergeError> {
        let img = &frames[i];
        let img = if half && what == Load::Align { fit(img, W / 2, W / 2, Filter::Box) } else { img.clone() };
        Ok(Frame::from_linear_rec2020(img, exposures.get(i).copied().flatten()))
    };
    stack_focus(frames.len(), &load, opts, &no_progress)
}

/// Largest distance, at the frame's corners, between where `h` maps reference pixels and where frame
/// `i` (magnified `zi`) sees them given the reference's magnification `zr`.
fn corner_error(h: &Homography, zi: f32, zr: f32) -> f64 {
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    let s = (zi / zr) as f64;
    [(0.0, 0.0), (W as f64, 0.0), (0.0, H as f64), (W as f64, H as f64)]
        .iter()
        .map(|&(x, y)| {
            let p = h.apply(Point::new(x, y));
            (p.x - (cx + (x - cx) * s)).hypot(p.y - (cy + (y - cy) * s))
        })
        .fold(0.0, f64::max)
}

/// The best single frame's worst band, each frame brought to the reference's geometry.
fn best_frame(frames: &[Rgb32f], zooms: &[f32], zr: f32, truth_ref: &Rgb32f) -> f32 {
    frames.iter().zip(zooms).map(|(f, &z)| worst(&band_errors(&rezoom(f, z, zr), truth_ref))).fold(f32::INFINITY, f32::min)
}

#[test]
fn stack_is_sharp_everywhere() {
    let n = 6;
    let (truth, frames) = synth::focus_bracket(W, H, n).unwrap();
    let t0 = std::time::Instant::now();
    let r = stack(&frames, &[], false, &FocusOptions::default()).unwrap();
    eprintln!("stack {W}x{H}×{n}: {:?}, reference {}", t0.elapsed(), r.reference);
    // the last frame is the most magnified: it sees the least, so it is the reference
    assert_eq!(r.reference, n - 1);
    assert!(r.skipped.is_empty());
    for (i, a) in r.alignments.iter().enumerate() {
        let e = corner_error(&a.h, zoom(i), zoom(n - 1));
        assert!(e < MAX_CORNER_ERROR, "frame {i}: {} corner error {e:.2} px", a.model);
        assert_eq!(a.model, if i == n - 1 { "reference" } else { "similarity" });
    }
    let truth_ref = rezoom(&truth, 1.0, zoom(n - 1));
    let stacked = band_errors(&r.image, &truth_ref);
    let zooms: Vec<f32> = (0..n).map(zoom).collect();
    let best = best_frame(&frames, &zooms, zoom(n - 1), &truth_ref);
    eprintln!("stack band errors {stacked:?}; best single frame's worst band {best}");
    assert!(worst(&stacked) < 0.75 * best, "stack's worst band {} vs the best frame's {best}", worst(&stacked));
}

#[test]
fn reversed_order_stacks_alike() {
    // a homography fallback fitted to features crowded into the sharp slice once made this order
    // worse than the best single frame
    let n = 6;
    let (truth, mut frames) = synth::focus_bracket(W, H, n).unwrap();
    frames.reverse();
    let r = stack(&frames, &[], false, &FocusOptions::default()).unwrap();
    assert_eq!(r.reference, 0);
    let zooms: Vec<f32> = (0..n).map(|i| zoom(n - 1 - i)).collect();
    for (i, a) in r.alignments.iter().enumerate() {
        let e = corner_error(&a.h, zooms[i], zooms[0]);
        assert!(e < MAX_CORNER_ERROR, "frame {i}: corner error {e:.2} px");
    }
    let truth_ref = rezoom(&truth, 1.0, zooms[0]);
    let stacked = worst(&band_errors(&r.image, &truth_ref));
    let best = best_frame(&frames, &zooms, zooms[0], &truth_ref);
    assert!(stacked < 0.75 * best, "stack's worst band {stacked} vs the best frame's {best}");
}

#[test]
fn small_versions_at_half_size_map_to_full_resolution() {
    let n = 5;
    let (_, frames) = synth::focus_bracket(W, H, n).unwrap();
    let r = stack(&frames, &[], true, &FocusOptions::default()).unwrap();
    assert_eq!(r.reference, n - 1);
    for (i, a) in r.alignments.iter().enumerate() {
        let e = corner_error(&a.h, zoom(i), zoom(n - 1));
        // features found at half the resolution
        assert!(e < 2.0 * MAX_CORNER_ERROR, "frame {i}: corner error {e:.2} px");
    }
}

#[test]
fn brightness_is_measured_from_the_pixels() {
    let n = 6;
    let (_, frames) = synth::focus_bracket(W, H, n).unwrap();
    let control = stack(&frames, &[], false, &FocusOptions::default()).unwrap();
    let mean = |img: &Rgb32f| img.data.iter().map(|p| p[1] as f64).sum::<f64>() / img.data.len() as f64;
    // each frame half a stop darker than the last, with no EXIF, with EXIF saying they are alike, and
    // with EXIF claiming steps the raw data doesn't have (auto ISO on a Sony A7R VI)
    let darker: Vec<Rgb32f> = frames.iter().enumerate().map(|(k, f)| f.map(|p| p.map(|v| v * 2f32.powf(-0.5 * k as f32)))).collect();
    let cases: [(&str, bool, Vec<Option<f64>>); 3] = [
        ("no EXIF", true, vec![]),
        ("EXIF alike", true, vec![Some(0.0); n]),
        ("EXIF steps, pixels alike", false, (0..n).map(|k| Some(-(k as f64) / 3.0)).collect()),
    ];
    for (what, dark, exif) in cases {
        let set = if dark { &darker } else { &frames };
        let r = stack(set, &exif, false, &FocusOptions::default()).unwrap();
        // the reference (the last, darkest frame) sets the result's brightness
        let want = if dark { 2f64.powf(-0.5 * (n - 1) as f64) } else { 1.0 };
        let ratio = mean(&r.image) / (mean(&control.image) * want);
        assert!((ratio - 1.0).abs() < 0.03, "{what}: brightness ratio {ratio}");
        for (k, ev) in r.ev.iter().enumerate() {
            let truth = if dark { 0.5 * (n - 1 - k) as f64 } else { 0.0 };
            assert!((ev - truth).abs() < 0.05, "{what}: frame {k} measured {ev:+.3} EV, truth {truth:+.3}");
        }
    }
}

#[test]
fn a_frame_that_aligns_to_nothing_is_left_out() {
    let n = 6;
    let (truth, mut frames) = synth::focus_bracket(W, H, n).unwrap();
    frames[3] = Rgb32f::from_fn(W, H, |_, _| [0.2, 0.2, 0.2]);
    let r = stack(&frames, &[], false, &FocusOptions::default()).unwrap();
    assert_eq!(r.skipped, vec![3]);
    assert_eq!(r.alignments[3].model, "failed");
    for i in [0, 1, 2, 4, 5] {
        let e = corner_error(&r.alignments[i].h, zoom(i), zoom(n - 1));
        assert!(e < MAX_CORNER_ERROR, "frame {i}: corner error {e:.2} px (bridged over the flat frame)");
    }
    let stacked = worst(&band_errors(&r.image, &rezoom(&truth, 1.0, zoom(n - 1))));
    assert!(stacked < 0.1, "stack's worst band {stacked}");
}

#[test]
fn without_auto_align_frames_stack_as_they_are() {
    let (_, frames) = synth::focus_bracket(W, H, 3).unwrap();
    let r = stack(&frames, &[], false, &FocusOptions { align: false }).unwrap();
    let models: Vec<&str> = r.alignments.iter().map(|a| a.model).collect();
    assert!(models.iter().all(|m| *m == "none" || *m == "reference"), "{models:?}");
    assert!(r.alignments.iter().all(|a| a.h == Homography::IDENTITY));
}

#[test]
fn frames_whose_orientation_tags_differ_still_stack() {
    // pixels are never turned; a camera pointing down flips its tag between frames
    let (_, frames) = synth::focus_bracket(W, H, 3).unwrap();
    let load = |i: usize, _: Load| -> Result<Frame, MergeError> {
        let mut f = Frame::from_linear_rec2020(frames[i].clone(), None);
        f.orientation = if i == 1 { Orientation::Rotate90 } else { Orientation::Normal };
        Ok(f)
    };
    let r = stack_focus(3, &load, &FocusOptions::default(), &no_progress).unwrap();
    assert_eq!(r.orientation, Orientation::Normal);
}

#[test]
fn bad_inputs_are_errors_not_panics() {
    let load = |_: usize, _: Load| -> Result<Frame, MergeError> { Err(MergeError::Decode("unused".into())) };
    assert!(matches!(stack_focus(1, &load, &FocusOptions::default(), &no_progress), Err(MergeError::TooFew(1, 2))));
    // no pixels, in the small or the full-size version
    for empty_at in [Load::Align, Load::Stack] {
        let load = |_: usize, what: Load| -> Result<Frame, MergeError> {
            let empty = what == empty_at || (empty_at == Load::Stack && what == Load::Reference);
            Ok(Frame::from_linear_rec2020(Rgb32f::new(if empty { 0 } else { 32 }, if empty { 5 } else { 24 }), None))
        };
        assert!(stack_focus(2, &load, &FocusOptions { align: false }, &no_progress).is_err(), "{empty_at:?}");
    }
    // sizes that differ
    let load = |i: usize, _: Load| -> Result<Frame, MergeError> { Ok(Frame::from_linear_rec2020(Rgb32f::new(32 + i, 24), None)) };
    assert!(matches!(stack_focus(2, &load, &FocusOptions { align: false }, &no_progress), Err(MergeError::Mismatch(_))));
}
