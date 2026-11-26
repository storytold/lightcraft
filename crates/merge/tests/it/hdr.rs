//! HDR merge on synthetic brackets: a procedural scene (scene-linear truth with real highlight
//! headroom) is "photographed" at several exposures with clipping, sensor noise, 14-bit
//! quantisation and small handheld shifts; the merge must recover the radiance.

use lightcraft_merge::frame::Frame;
use lightcraft_merge::hdr::{Deghost, HdrOptions, merge_hdr};
use lightcraft_merge::no_progress;
use lightcraft_raster::Rgb32f;

const W: usize = 640;
const H: usize = 420;
const M: usize = 16;

struct Rng(u64);
impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f32 {
        let (a, b) = (self.unit().max(1e-12), self.unit());
        ((-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()) as f32
    }
}

/// Scene radiance (with margin), scaled so the median is ≈ 0.18 at exposure 1.
fn truth() -> Rgb32f {
    let scene = lightcraft_scenes::demo_library().into_iter().find(|s| s.kind == lightcraft_scenes::Kind::OceanSunset).unwrap();
    let img = scene.render(W + 2 * M, H + 2 * M);
    let mut l: Vec<f32> = img.data.iter().map(|p| p[1]).collect();
    l.sort_by(|a, b| a.total_cmp(b));
    let s = 0.18 / l[l.len() / 2].max(1e-6);
    img.map(|p| p.map(|v| v * s))
}

/// A disk of radiance `val` centred at (cx, cy), radius r (truth coordinates).
fn with_object(t: &Rgb32f, cx: f32, cy: f32, r: f32, val: [f32; 3]) -> Rgb32f {
    Rgb32f::from_fn(t.width, t.height, |x, y| {
        let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
        if d < r { val } else { t.get(x, y) }
    })
}

/// Photograph `t` at exposure `k`, content shifted by (dx, dy).
fn shoot(t: &Rgb32f, k: f32, dx: f32, dy: f32, seed: u64) -> Frame {
    let mut rng = Rng(seed * 0x9e37_79b9 + 7);
    let mut img = Rgb32f::from_fn(W, H, |x, y| t.sample_bilinear(x as f32 + 0.5 + M as f32 - dx, y as f32 + 0.5 + M as f32 - dy).map(|v| v * k));
    // shot + read noise, clipping at the white level, 14-bit quantisation
    for p in img.data.iter_mut() {
        for c in 0..3 {
            let s = p[c].max(0.0);
            let sigma = (1e-4 * s + 4e-8).sqrt();
            p[c] = ((s + sigma * rng.gauss()).clamp(0.0, 1.0) * 16383.0).round() / 16383.0;
        }
    }
    let mut f = Frame::from_linear_rec2020(img, Some((k as f64).log2()));
    f.raw = true;
    f.clip = 1.0;
    f
}

fn log_err(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|c| ((a[c] + 0.003).ln() - (b[c] + 0.003).ln()).abs()).fold(0.0, f32::max)
}

fn pct(mut v: Vec<f32>, p: f32) -> f32 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f32 * p) as usize]
}

#[test]
fn bracket_merge_recovers_radiance_and_alignment() {
    let t = truth();
    let shifts = [(3.25f32, -2.0f32), (0.0, 0.0), (-4.0, 1.5)];
    let ks = [0.25f32, 1.0, 4.0];
    let frames: Vec<Frame> = (0..3).map(|i| shoot(&t, ks[i], shifts[i].0, shifts[i].1, i as u64 + 1)).collect();
    let t0 = std::time::Instant::now();
    let r = merge_hdr(frames, &HdrOptions { align: true, deghost: Deghost::None }, &no_progress).unwrap();
    eprintln!("merge {W}x{H}: {:?}", t0.elapsed());
    assert_eq!(r.reference, 1);
    // exposure estimates within 0.02 EV of the truth
    for (i, k) in ks.iter().enumerate() {
        assert!((r.ev[i] - (*k as f64).log2()).abs() < 0.02, "ev {:?}", r.ev);
    }
    // alignment: the reference → frame mapping is the content shift
    for i in [0, 2] {
        let a = &r.alignments[i];
        for (x, y) in [(50.0, 50.0), (W as f64 - 50.0, H as f64 - 50.0), (320.0, 200.0)] {
            let p = a.h.apply(lightcraft_geom::Point::new(x, y));
            let err = ((p.x - x - shifts[i].0 as f64).powi(2) + (p.y - y - shifts[i].1 as f64).powi(2)).sqrt();
            assert!(err < 0.3, "frame {i} ({}, {} inliers, rms {:.3}): alignment error {err:.3} px", a.model, a.inliers, a.rms);
        }
    }
    // radiance vs truth (reference geometry), away from the borders, inside the bracket's range
    let mut errs = Vec::new();
    let mut recovered = Vec::new();
    let mut bias = 0f64;
    for y in 12..H - 12 {
        for x in 12..W - 12 {
            let want = t.get(x + M, y + M);
            let m = want[0].max(want[1]).max(want[2]);
            if !(0.002..3.5).contains(&m) {
                continue;
            }
            let e = log_err(r.radiance.get(x, y), want);
            let g = r.radiance.get(x, y)[1];
            bias += ((g + 0.003).ln() - (want[1] + 0.003).ln()) as f64;
            errs.push(e);
            if m > 1.2 {
                recovered.push(e);
            }
        }
    }
    let n_err = errs.len();
    eprintln!("mean signed log error (green): {:.4}", bias / n_err as f64);
    let (med, p95) = (pct(errs.clone(), 0.5), pct(errs, 0.95));
    eprintln!("log error: median {med:.4}, p95 {p95:.4}; highlights recovered: {} px", recovered.len());
    // noise-limited (the error is the worst of three channels); no bias
    assert!(med < 0.03 && p95 < 0.1, "median {med}, p95 {p95}");
    assert!((bias / n_err as f64).abs() < 0.01);
    assert!(recovered.len() > 200, "the scene should have highlights above the reference's clip");
    let rmed = pct(recovered, 0.5);
    assert!(rmed < 0.05, "recovered highlights: median log error {rmed}");
}

/// The result doesn't depend on the order the frames come in: every permutation of a bracket gives the same radiance and
/// deghost overlay, bit for bit, and the same per-frame results for each frame. (Real brackets merged in the app's
/// selection order and in the CLI's file order differed on 0.6 % of their pixels: alignment seeds followed the input
/// positions.)
#[test]
fn merge_does_not_depend_on_the_frame_order() {
    let t = truth();
    let shifts = [(3.25f32, -2.0f32), (0.0, 0.0), (-4.0, 1.5)];
    let ks = [0.25f32, 1.0, 4.0];
    let shot: Vec<Frame> = (0..3).map(|i| shoot(&t, ks[i], shifts[i].0, shifts[i].1, i as u64 + 1)).collect();
    let opts = HdrOptions { align: true, deghost: Deghost::Medium };
    let base = merge_hdr(shot.clone(), &opts, &no_progress).unwrap();
    assert_eq!(base.reference, 1);
    for perm in [[2usize, 0, 1], [1, 2, 0], [0, 2, 1]] {
        let r = merge_hdr(perm.iter().map(|&i| shot[i].clone()).collect(), &opts, &no_progress).unwrap();
        assert!(r.radiance.data == base.radiance.data, "{perm:?}: the radiance differs");
        assert!(r.ghost.data == base.ghost.data, "{perm:?}: the deghost overlay differs");
        assert_eq!(perm[r.reference], base.reference, "{perm:?}");
        for (p, &i) in perm.iter().enumerate() {
            assert_eq!(r.ev[p], base.ev[i], "{perm:?}");
            assert_eq!(r.alignments[p].h, base.alignments[i].h, "{perm:?}");
        }
    }
}

/// Pixels demosaiced next to clipped photosites read low in the clipped channel (real ARW brackets: the brightest
/// frame's green 3–5 % low within 2 px of clipped pixels, linear to 1 % elsewhere); the merge must not take them from
/// that frame, or bright windows get magenta fringes. Here the +2 EV frame's green is 5 % low around its clipped areas.
#[test]
fn pixels_next_to_clipped_ones_do_not_tint_the_merge() {
    let t = truth();
    let ks = [0.25f32, 1.0, 4.0];
    let mut frames: Vec<Frame> = (0..3).map(|i| shoot(&t, ks[i], 0.0, 0.0, i as u64 + 1)).collect();
    let (w, h) = (frames[2].image.width, frames[2].image.height);
    let clipped = |img: &Rgb32f, x: usize, y: usize| img.data[y * w + x].iter().any(|&v| v >= 1.0);
    let mut ring = vec![false; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let img = &frames[2].image;
            ring[y * w + x] = !clipped(img, x, y) && (y - 1..=y + 1).any(|yy| (x - 1..=x + 1).any(|xx| clipped(img, xx, yy)));
        }
    }
    for (p, &r) in frames[2].image.data.iter_mut().zip(&ring) {
        if r {
            p[1] *= 0.95;
        }
    }
    let r = merge_hdr(frames, &HdrOptions { align: false, deghost: Deghost::None }, &no_progress).unwrap();
    let ratio = |p: [f32; 3]| p[1] / (p[0] + p[2]);
    let mut devs = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let want = t.get(x + M, y + M);
            if ring[y * w + x] && want[0] > 0.01 && want[2] > 0.01 {
                devs.push(ratio(r.radiance.get(x, y)) / ratio(want) - 1.0);
            }
        }
    }
    assert!(devs.len() > 500, "ring pixels: {}", devs.len());
    let med = pct(devs, 0.5);
    assert!(med.abs() < 0.01, "green against red and blue on the ring: {:+.2} %", 100.0 * med);
}

#[test]
fn deghost_removes_a_moving_object() {
    let t = truth();
    let ks = [0.25f32, 1.0, 4.0];
    // a dark object at a different place in each frame (truth coordinates)
    let pos = [(200.0f32, 260.0f32), (330.0, 260.0), (460.0, 260.0)];
    let frames = |_: ()| -> Vec<Frame> {
        (0..3).map(|i| shoot(&with_object(&t, pos[i].0, pos[i].1, 28.0, [0.01, 0.012, 0.02]), ks[i], 0.0, 0.0, 10 + i as u64)).collect()
    };
    let ref_truth = with_object(&t, pos[1].0, pos[1].1, 28.0, [0.01, 0.012, 0.02]);
    let err_at = |img: &Rgb32f, cx: f32, cy: f32| -> f32 {
        let mut v = Vec::new();
        for y in (cy - 20.0) as usize..(cy + 20.0) as usize {
            for x in (cx - 20.0) as usize..(cx + 20.0) as usize {
                let (tx, ty) = (x - M, y - M);
                v.push(log_err(img.get(tx, ty), ref_truth.get(x, y)));
            }
        }
        v.iter().sum::<f32>() / v.len() as f32
    };
    let none = merge_hdr(frames(()), &HdrOptions { align: false, deghost: Deghost::None }, &no_progress).unwrap();
    let high = merge_hdr(frames(()), &HdrOptions { align: false, deghost: Deghost::High }, &no_progress).unwrap();
    let med = merge_hdr(frames(()), &HdrOptions { align: false, deghost: Deghost::Medium }, &no_progress).unwrap();
    for (cx, cy) in [pos[0], pos[2]] {
        let (en, eh, em) = (err_at(&none.radiance, cx, cy), err_at(&high.radiance, cx, cy), err_at(&med.radiance, cx, cy));
        eprintln!("ghost at ({cx},{cy}): none {en:.3}, medium {em:.3}, high {eh:.3}");
        assert!(en > 0.15, "without deghosting the object leaves a ghost ({en})");
        assert!(eh < 0.08 && em < 0.12, "deghosting removes it (high {eh}, medium {em})");
        // the overlay marks the ghost
        let g = high.ghost.get((cx - M as f32) as usize, (cy - M as f32) as usize);
        assert!(g > 0.5, "overlay at the ghost: {g}");
    }
    assert!(none.ghost.data.is_empty());
    let mean = high.ghost.data.iter().sum::<f32>() / high.ghost.data.len() as f32;
    assert!(mean < 0.08, "the overlay is local (mean {mean})");
}

/// A dark night sky (≈ 0.3 % of the reference's white) with a soft star of peak `peak` (Gaussian, σ = 3 px) at (cx, cy)
/// in truth coordinates.
fn night_with_star(cx: f32, cy: f32, peak: f32) -> Rgb32f {
    Rgb32f::from_fn(W + 2 * M, H + 2 * M, |x, y| {
        let sky = 0.003 * (1.0 + 0.3 * (x as f32 / (W + 2 * M) as f32));
        let d2 = (x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2);
        [sky + peak * (-d2 / 18.0).exp(); 3]
    })
}

/// Night brackets of a star that moves between the frames (the sky turns during a tripod bracket): its core clips in the
/// reference and in the brighter frame, not in the darkest one.
fn moving_star_frames(xs: [f32; 3]) -> Vec<Frame> {
    let ks = [0.0625f32, 1.0, 8.0];
    (0..3).map(|i| shoot(&night_with_star(xs[i], 210.0 + M as f32, 10.0), ks[i], 0.0, 0.0, 30 + i as u64)).collect()
}

#[test]
fn a_clipped_star_of_the_reference_is_not_erased_by_frames_where_it_moved() {
    let xs = [280.0 + M as f32, 320.0 + M as f32, 360.0 + M as f32];
    let r = merge_hdr(moving_star_frames(xs), &HdrOptions { align: false, deghost: Deghost::None }, &no_progress).unwrap();
    assert_eq!(r.reference, 1);
    // the reference saw the star clipped here, so the scene is at least that bright; the frames where the star had moved
    // read the dark sky there and must not replace it (which left a black hole in real night brackets)
    let core = r.radiance.get(320, 210);
    let m = core[0].max(core[1]).max(core[2]);
    assert!(m > 0.9, "the reference's clipped star core was erased: {core:?}");
}

#[test]
fn deghost_removes_a_bright_frames_ghost_in_a_dark_sky() {
    let xs = [280.0 + M as f32, 320.0 + M as f32, 360.0 + M as f32];
    let r = merge_hdr(moving_star_frames(xs), &HdrOptions { align: false, deghost: Deghost::High }, &no_progress).unwrap();
    // where only the brightest frame had the star, the sky is what the reference saw: no ring of its unclipped halo
    let mut worst = 0f32;
    for y in 196..224 {
        for x in 346..374 {
            let p = r.radiance.get(x, y);
            worst = worst.max(p[1]);
        }
    }
    assert!(worst < 0.02, "the brightest frame's star is left as a ghost (peak {worst})");
    assert!(r.ghost.get(360, 210) > 0.5, "the overlay marks the ghost");
}
