//! HDR merge sharpness at a real frame size: an analytic scene (fine texture up to 0.35 cycles per
//! pixel, edges, a bright area that clips) photographed handheld at three exposures, each frame
//! turned a little and moved by a fraction of a pixel. The merge must find the motion to a fraction
//! of a pixel (alignment runs on ≤ 1200 px versions of these 3000 px frames) and keep the scene's
//! detail. Before the full-size refinement and the Lanczos-3 warp: corner errors 0.43 and 6.2 px (the
//! bright frame, over half clipped, wasn't aligned at all) and 86 % of the detail kept; after:
//! 0.014 and 0.11 px, 98.8 %.

use lightcraft_geom::{Homography, Point};
use lightcraft_merge::frame::Frame;
use lightcraft_merge::hdr::{Deghost, HdrOptions, merge_hdr};
use lightcraft_merge::metrics::{detail, detail_gain, detail_ratio};
use lightcraft_merge::no_progress;
use lightcraft_raster::Rgb32f;

mod common;
use common::{Rng, corner_error};

const W: usize = 3000;
const H: usize = 2000;

/// Scene radiance at continuous coordinates (≈ 0.18 middle grey; ×12 in a disc that clips in the
/// middle and bright frames).
fn scene(x: f64, y: f64) -> f32 {
    let mut t = 0.0;
    for k in 0..12 {
        let a = k as f64 * 2.399_963;
        let f = 0.015 + 0.03 * k as f64; // up to 0.345 cycles per pixel
        t += ((x * a.cos() + y * a.sin()) * f * std::f64::consts::TAU + k as f64 * 1.3).sin() / 12f64.sqrt();
    }
    // a few hard-ish edges (0.7 px wide)
    let edge = |d: f64| 1.0 / (1.0 + (-d / 0.35).exp());
    let e = edge(((x - 700.0) * 0.8 + (y - 500.0) * 0.6) % 400.0 - 200.0) * 0.5;
    let v = 0.18 * (0.5 * t + e).exp();
    let bright = if (x - 2200.0).powi(2) + (y - 600.0).powi(2) < 300f64.powi(2) { 12.0 } else { 1.0 };
    (v * bright) as f32
}

/// A similarity turning by `deg` about the frame centre and moving by (dx, dy): frame pixel → scene.
fn motion(deg: f64, dx: f64, dy: f64) -> Homography {
    let (c, s) = (deg.to_radians().cos(), deg.to_radians().sin());
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    Homography([c, -s, cx - c * cx + s * cy + dx, s, c, cy - s * cx - c * cy + dy, 0.0, 0.0, 1.0])
}

fn shoot(k: f32, m: &Homography, seed: u64) -> Frame {
    let mut rng = Rng(seed * 0x9e37_79b9 + 11);
    let mut img = Rgb32f::from_fn(W, H, |x, y| {
        let p = m.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
        [scene(p.x, p.y) * k; 3]
    });
    for p in img.data.iter_mut() {
        let s = p[0].max(0.0);
        let v = ((s + (2e-5 * s + 2e-8).sqrt() * rng.gauss()).clamp(0.0, 1.0) * 16383.0).round() / 16383.0;
        *p = [v; 3];
    }
    let mut f = Frame::from_linear_rec2020(img, Some((k as f64).log2()));
    f.raw = true;
    f.clip = 1.0;
    f
}

#[test]
fn handheld_bracket_merges_sharp() {
    let motions = [motion(0.11, 2.31, -1.73), Homography::IDENTITY, motion(-0.07, -3.13, 2.62)];
    let ks = [0.25f32, 1.0, 4.0];
    let frames: Vec<Frame> = (0..3).map(|i| shoot(ks[i], &motions[i], i as u64 + 1)).collect();
    let t0 = std::time::Instant::now();
    let r = merge_hdr(frames, &HdrOptions { align: true, deghost: Deghost::None }, &no_progress).unwrap();
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(r.reference, 1);
    // alignment: reference → frame is the inverse of the frame's motion
    let errs: Vec<f64> = [0, 2].iter().map(|&i| corner_error(&r.alignments[i].h, &motions[i].inverse().unwrap(), W as f64, H as f64)).collect();
    // detail against the scene itself, away from the edges and the clipped disc
    let truth = Rgb32f::from_fn(W, H, |x, y| [scene(x as f64 + 0.5, y as f64 + 0.5); 3]);
    let keep = |x: usize, y: usize| {
        x > 40 && y > 40 && x < W - 40 && y < H - 40 && ((x as f64 - 2200.0).powi(2) + (y as f64 - 600.0).powi(2)) > 330f64.powi(2)
    };
    let ratio = detail_ratio(&detail(&r.radiance), &detail(&truth), keep).unwrap();
    // the same without noise counting: the merge's fine detail projected onto the scene's
    let gain = detail_gain(&r.radiance, &truth, keep).unwrap();
    eprintln!("detail gain {gain:.3}");
    eprintln!("alignment corner error {errs:.3?} px, detail kept {ratio:.3}, {secs:.1} s");
    assert!(errs.iter().all(|e| *e < 0.2), "alignment corner error {errs:?} px");
    assert!(ratio > 0.9, "detail kept {ratio}");
}

/// The merge keeps the reference's fine detail where the reference is well exposed, even when the other frames are
/// a little soft (resampled, aligned to a fraction of a pixel): here blurred σ = 0.7 px and merged as they are.
#[test]
fn soft_frames_dont_soften_the_merge() {
    let ks = [0.25f32, 1.0, 4.0];
    let frames: Vec<Frame> = (0..3)
        .map(|i| {
            let mut f = shoot(ks[i], &Homography::IDENTITY, i as u64 + 1);
            if i != 1 {
                f.image = lightcraft_raster::blur::gaussian(&f.image, 0.7);
            }
            f
        })
        .collect();
    let r = merge_hdr(frames, &HdrOptions { align: false, deghost: Deghost::None }, &no_progress).unwrap();
    let truth = Rgb32f::from_fn(W, H, |x, y| [scene(x as f64 + 0.5, y as f64 + 0.5); 3]);
    // where the reference is well exposed (scene radiance 0.05 to 0.6 of its clip), away from the bright disc
    let keep = |x: usize, y: usize| {
        let v = truth.get(x, y)[0];
        x > 40 && y > 40 && x < W - 40 && y < H - 40 && (0.05..0.6).contains(&v)
    };
    let gain = detail_gain(&r.radiance, &truth, keep).unwrap();
    eprintln!("soft frames: detail gain {gain:.3}");
    // 0.656 without the reference's detail, 0.971 with it
    assert!(gain > 0.95, "detail gain {gain}");
}

/// Where something moved between the frames (a leaf in the wind) and nothing removes the ghost, the merge is a blend
/// of what the frames saw there; the reference's detail must not be laid over that blend. It was: a dark disc moving
/// 12 px per frame came out with hard, stepped double edges, brighter and darker than any frame saw.
#[test]
fn moving_things_get_no_stepped_edges() {
    const R: f64 = 60.0;
    let leaf_x = |i: usize| 1000.0 + 12.0 * i as f64;
    // the scene with the leaf (radiance × 0.3, a 0.7 px edge) where frame i saw it
    let seen = |i: usize, x: f64, y: f64| {
        let d = ((x - leaf_x(i)).powi(2) + (y - 1200.0).powi(2)).sqrt() - R;
        scene(x, y) * (1.0 - 0.7 / (1.0 + (d / 0.35).exp()) as f32)
    };
    let ks = [0.25f32, 1.0, 4.0];
    let frames: Vec<Frame> = (0..3)
        .map(|i| {
            let mut rng = Rng(i as u64 * 0x9e37_79b9 + 11);
            let mut img = Rgb32f::from_fn(W, H, |x, y| [seen(i, x as f64 + 0.5, y as f64 + 0.5) * ks[i]; 3]);
            for p in img.data.iter_mut() {
                let s = p[0].max(0.0);
                let v = ((s + (2e-5 * s + 2e-8).sqrt() * rng.gauss()).clamp(0.0, 1.0) * 16383.0).round() / 16383.0;
                *p = [v; 3];
            }
            let mut f = Frame::from_linear_rec2020(img, Some((ks[i] as f64).log2()));
            f.raw = true;
            f.clip = 1.0;
            f
        })
        .collect();
    let r = merge_hdr(frames, &HdrOptions { align: false, deghost: Deghost::None }, &no_progress).unwrap();
    // in the band the leaf swept, every merged pixel lies between what the frames saw (10 % for noise)
    let (mut out, mut n) = (0usize, 0usize);
    for y in 1120..1280 {
        for x in 930..1100 {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let v: Vec<f32> = (0..3).map(|i| seen(i, fx, fy)).collect();
            let (lo, hi) = (v.iter().copied().fold(f32::INFINITY, f32::min), v.iter().copied().fold(0.0, f32::max));
            if hi - lo < 0.2 * hi {
                continue;
            }
            let m = r.radiance.get(x, y)[1];
            n += 1;
            if m > hi * 1.1 || m < lo * 0.9 {
                out += 1;
            }
        }
    }
    let share = out as f64 / n.max(1) as f64;
    eprintln!("moving leaf: {out} of {n} pixels outside what the frames saw ({:.1} %)", share * 100.0);
    assert!(n > 1000, "too few pixels where the leaf moved: {n}");
    assert!(share < 0.01, "{:.1} % of the pixels where the leaf moved are outside what the frames saw", share * 100.0);
}

/// The same moving leaf in colour (a blue leaf over a yellow-green background): where it moved, the merge keeps a
/// blend of the colours the frames saw and adds no colour fringe of its own. Every frame weighs a pixel's three
/// channels alike and the detail pass scales them by one factor, so each merged channel ratio (R / G, B / G) lies
/// between the frames' ratios there.
#[test]
fn moving_things_get_no_colour_fringes() {
    const R: f64 = 60.0;
    const BG: [f32; 3] = [0.9, 1.0, 0.45];
    const LEAF: [f32; 3] = [0.12, 0.25, 0.4];
    let leaf_x = |i: usize| 1000.0 + 12.0 * i as f64;
    let seen = |i: usize, x: f64, y: f64| {
        let d = ((x - leaf_x(i)).powi(2) + (y - 1200.0).powi(2)).sqrt() - R;
        let m = (1.0 / (1.0 + (d / 0.35).exp())) as f32;
        let s = scene(x, y);
        [0, 1, 2].map(|c| s * (BG[c] * (1.0 - m) + LEAF[c] * m))
    };
    let ks = [0.25f32, 1.0, 4.0];
    let frames: Vec<Frame> = (0..3)
        .map(|i| {
            let mut rng = Rng(i as u64 * 0x9e37_79b9 + 23);
            let mut img = Rgb32f::from_fn(W, H, |x, y| seen(i, x as f64 + 0.5, y as f64 + 0.5).map(|v| v * ks[i]));
            for p in img.data.iter_mut() {
                *p = p.map(|s| {
                    let s = s.max(0.0);
                    ((s + (2e-5 * s + 2e-8).sqrt() * rng.gauss()).clamp(0.0, 1.0) * 16383.0).round() / 16383.0
                });
            }
            let mut f = Frame::from_linear_rec2020(img, Some((ks[i] as f64).log2()));
            f.raw = true;
            f.clip = 1.0;
            f
        })
        .collect();
    let r = merge_hdr(frames, &HdrOptions { align: false, deghost: Deghost::None }, &no_progress).unwrap();
    // in the band the leaf swept, below the brightest frame's clip: each merged ratio within the frames' (10 % for noise)
    let (mut out, mut n) = (0usize, 0usize);
    for y in 1120..1280 {
        for x in 930..1100 {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let v: Vec<[f32; 3]> = (0..3).map(|i| seen(i, fx, fy)).collect();
            if v.iter().any(|p| p[1] * ks[2] > 0.75) {
                continue;
            }
            let m = r.radiance.get(x, y);
            let mut swept = false;
            let mut fringe = false;
            for c in [0, 2] {
                let q: Vec<f32> = v.iter().map(|p| p[c] / p[1]).collect();
                let (lo, hi) = (q.iter().copied().fold(f32::INFINITY, f32::min), q.iter().copied().fold(0.0, f32::max));
                if hi - lo < 0.2 * hi {
                    continue;
                }
                swept = true;
                let qm = m[c] / m[1].max(1e-9);
                fringe |= qm > hi * 1.1 || qm < lo * 0.9;
            }
            if swept {
                n += 1;
                out += usize::from(fringe);
            }
        }
    }
    let share = out as f64 / n.max(1) as f64;
    eprintln!("moving colour leaf: {out} of {n} pixels outside the frames' colours ({:.1} %)", share * 100.0);
    assert!(n > 500, "too few pixels where the leaf moved: {n}");
    assert!(share < 0.01, "{:.1} % of the pixels where the leaf moved have a colour no frame saw", share * 100.0);
}

/// The reference's clipped highlights don't leak into their surroundings: next to the bright disc (clipped in the
/// reference, its radiance from the darker frame) the merge stays as bright as the scene.
#[test]
fn no_bright_rim_around_clipped_highlights() {
    let ks = [0.25f32, 1.0, 4.0];
    let frames: Vec<Frame> = (0..3).map(|i| shoot(ks[i], &Homography::IDENTITY, i as u64 + 1)).collect();
    let r = merge_hdr(frames, &HdrOptions { align: false, deghost: Deghost::None }, &no_progress).unwrap();
    // mean merged / true luminance in a ring 2 to 10 px outside the disc
    let (mut sum, mut n) = (0f64, 0usize);
    for y in 250..950 {
        for x in 1850..2550 {
            let d = ((x as f64 + 0.5 - 2200.0).powi(2) + (y as f64 + 0.5 - 600.0).powi(2)).sqrt();
            if (302.0..310.0).contains(&d) {
                let t = scene(x as f64 + 0.5, y as f64 + 0.5) as f64;
                sum += r.radiance.get(x, y)[1] as f64 / t.max(1e-6);
                n += 1;
            }
        }
    }
    let ratio = sum / n as f64;
    eprintln!("next to the clipped disc: merge / scene {ratio:.3}");
    assert!((ratio - 1.0).abs() < 0.03, "merge / scene next to the clipped disc: {ratio}");
}
