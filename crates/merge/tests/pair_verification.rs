//! Issue #1765: deterministic correspondences isolate verification from feature detection.
//! These are original procedural fixtures, not the reporter's waterfall photos.

use lightcraft_geom::{Homography, Point};
use lightcraft_merge::features::{Features, Keypoint, match_features};
use lightcraft_merge::pano::camera::match_pairs;
use lightcraft_merge::ransac::{Model, Rng, ransac};

fn correspondences(n: usize, inliers: usize, seed: u64, h: Homography) -> [Features; 2] {
    let mut rng = Rng::new(seed);
    let mut a = Features::default();
    let mut b = Features::default();
    for k in 0..n {
        let p = Point::new(80.0 + rng.unit() * 640.0, 60.0 + rng.unit() * 480.0);
        let q = if k < inliers { h.apply(p) } else { Point::new(rng.unit() * 800.0, rng.unit() * 600.0) };
        let desc = std::array::from_fn(|_| rng.unit() as f32);
        for (f, p) in [(&mut a, p), (&mut b, q)] {
            f.points.push(Keypoint { x: p.x as f32, y: p.y as f32, level: 0, angle: 0.0, strength: 1.0 });
            f.desc.push(desc);
        }
    }
    [a, b]
}

fn fit(f: &[Features; 2]) -> Option<lightcraft_merge::ransac::Fit> {
    let matches = match_features(&f[0], &f[1], 0.8);
    assert_eq!(matches.len(), f[0].len());
    let points = |side: usize| -> Vec<Point> {
        matches
            .iter()
            .map(|&(i, j)| {
                let p = f[side].points[if side == 0 { i } else { j }];
                Point::new(p.x as f64, p.y as f64)
            })
            .collect()
    };
    ransac(Model::Homography, &points(0), &points(1), 3.0, 12, 1)
}

#[test]
fn moving_scene_regime_is_rejected_despite_a_recovered_homography() {
    let h = Homography([1.0, 0.01, 23.0, -0.01, 1.0, 7.0, 0.00001, -0.00002, 1.0]);
    for (n, inliers) in [(104, 31), (136, 36), (100, 30), (140, 30)] {
        let f = correspondences(n, inliers, 1765, h);
        let recovered = fit(&f).expect("RANSAC recovers the known transform");
        assert_eq!(recovered.inliers.len(), inliers);
        assert!(recovered.rms < 0.001);
        for p in [Point::new(100.0, 100.0), Point::new(700.0, 500.0)] {
            assert!(lightcraft_merge::ransac::transfer_error2(&recovered.h, p, h.apply(p)) < 0.001);
        }
        eprintln!("known homography: {inliers}/{n}, rms={:.6}, accepted={}", recovered.rms, !match_pairs(&f, &[1.0; 2]).is_empty());
        // Keep the conservative gate until a policy distinguishes this from a coherent moving
        // object or a repeated patch. Lowering only the fraction does not establish that.
        assert!(match_pairs(&f, &[1.0; 2]).is_empty());
        assert!(inliers as f64 > 8.0 + 0.15 * n as f64, "the proposed lower slope admits it");
    }
}

#[test]
fn a_repeated_patch_can_pass_the_lower_slope_with_the_wrong_camera_motion() {
    // A copied, textured panel covers the image: 36 distinctive features agree with a false
    // 160 px translation. All other correspondences disagree. Spatial spread cannot tell a
    // repeated panel from static scene support using these observations alone.
    let false_motion = Homography([1.0, 0.0, 160.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let f = correspondences(136, 36, 1765, false_motion);
    let r = fit(&f).unwrap();
    eprintln!("repeated panel: {}/136, rms={:.6}, lower slope accepts=true", r.inliers.len(), r.rms);
    assert_eq!(r.inliers.len(), 36);
    assert!(r.rms < 0.001);
    assert!(r.inliers.len() as f64 > 8.0 + 0.15 * 136.0);
    assert!(match_pairs(&f, &[1.0; 2]).is_empty());
}

#[test]
fn unrelated_low_overlap_duplicates_and_ambiguous_descriptors() {
    let h = Homography([1.0, 0.0, 30.0, 0.0, 1.0, 12.0, 0.0, 0.0, 1.0]);
    for seed in 1..=8 {
        for inliers in [0, 8] {
            let f = correspondences(120, inliers, seed, h);
            assert!(match_pairs(&f, &[1.0; 2]).is_empty(), "seed {seed}, {inliers} inliers");
        }
    }
    let identity = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let mut f = correspondences(100, 100, 1765, identity);
    assert_eq!(match_pairs(&f, &[1.0; 2]).len(), 1, "duplicates remain one verified pair");
    for side in &mut f {
        side.desc.fill([1.0; 64]);
    }
    assert!(match_pairs(&f, &[1.0; 2]).is_empty(), "identical repeating descriptors are ambiguous");
}

#[test]
fn translated_camera_with_multiple_depths_does_not_establish_a_panorama() {
    // Pinhole cameras with f=800, translated horizontally by 0.2 world units. Points
    // span depths 1..5, so their parallax is 32..160 px and no rotation explains them.
    let identity = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let mut f = correspondences(136, 136, 1765, identity);
    let mut rng = Rng::new(987);
    for p in &mut f[1].points {
        p.x -= (160.0 / (1.0 + 4.0 * rng.unit())) as f32;
    }
    let r = fit(&f);
    eprintln!(
        "translated camera, varying depths: fit={:?}, accepted={}",
        r.as_ref().map(|r| (r.inliers.len(), r.rms)),
        !match_pairs(&f, &[1.0; 2]).is_empty()
    );
    assert!(match_pairs(&f, &[1.0; 2]).is_empty());
    assert!(r.unwrap().inliers.len() as f64 > 8.0 + 0.15 * 136.0, "the lower slope accepts genuine parallax");
}

#[test]
fn diagnostics_keep_rejected_and_unmatched_pairs_in_input_order() {
    use lightcraft_merge::pano::camera::{PairStatus, match_pairs_with_diagnostics};
    let h = Homography([1.0, 0.0, 23.0, 0.0, 1.0, 7.0, 0.0, 0.0, 1.0]);
    let [a, b] = correspondences(104, 31, 1765, h);
    let (pairs, diagnostics) = match_pairs_with_diagnostics(&[a, b, Features::default()], &[1.0; 3]);
    assert!(pairs.is_empty());
    assert_eq!(diagnostics.iter().map(|d| (d.i, d.j)).collect::<Vec<_>>(), [(0, 1), (0, 2), (1, 2)]);
    assert_eq!((diagnostics[0].matches, diagnostics[0].inliers, diagnostics[0].status), (104, Some(31), PairStatus::Rejected));
    assert_eq!((diagnostics[1].matches, diagnostics[1].inliers, diagnostics[1].status), (0, None, PairStatus::InsufficientMatches));
    let f = correspondences(120, 0, 1, h);
    let (_, diagnostics) = match_pairs_with_diagnostics(&f, &[1.0; 2]);
    assert_eq!((diagnostics[0].inliers, diagnostics[0].status), (None, PairStatus::NoModel));
}
