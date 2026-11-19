use super::*;

fn model(out: [f32; 3]) -> Model {
    Model::new(&crate::onnx::read_bytes(&crate::synthetic::defringe_onnx(out)).unwrap()).unwrap()
}

/// A grey picture with a white block on the left whose right edge has a 2 px purple fringe, the same on every row.
fn fringed(w: usize, h: usize) -> (Vec<[f32; 3]>, Vec<bool>) {
    let px = |x: usize| match x {
        0..100 => [1.0, 1.0, 1.0],
        100..102 => [0.45, 0.08, 0.6],
        _ => [0.18, 0.18, 0.18],
    };
    let rgb: Vec<[f32; 3]> = (0..w * h).map(|i| px(i % w)).collect();
    let clip = vec![false; w * h];
    (rgb, clip)
}

fn chroma(p: [f32; 3]) -> f32 {
    let l = oklab(p);
    (l[1] * l[1] + l[2] * l[2]).sqrt()
}

fn run(m: &Model, rgb: &[[f32; 3]], clip: &[bool], w: usize, h: usize) -> Vec<(u32, [f32; 3])> {
    m.correct(rgb, clip, w, h, &|_, _| {}, &AtomicBool::new(false)).unwrap()
}

#[test]
fn a_network_that_asks_for_nothing_changes_nothing() {
    let (rgb, clip) = fringed(300, 280);
    assert!(run(&model([0.0, 0.0, 0.0]), &rgb, &clip, 300, 280).is_empty());
    assert!(run(&model([5.0, -5.0, -20.0]), &rgb, &clip, 300, 280).is_empty());
}

#[test]
fn a_purple_fringe_is_taken_toward_the_grey_around_it_and_nothing_else_moves() {
    let (w, h) = (300, 280);
    let (rgb, clip) = fringed(w, h);
    // purple is +a −b in OkLab: push the other way, everywhere, at full gate
    let changed = run(&model([-3.0, 3.0, 10.0]), &rgb, &clip, w, h);
    assert!(!changed.is_empty());
    for &(i, p) in &changed {
        let (x, i) = (i as usize % w, i as usize);
        assert!((100..102).contains(&x), "pixel at x = {x} changed");
        assert!(chroma(p) < chroma(rgb[i]) * 0.9, "{:?} → {p:?}", rgb[i]);
        assert!((oklab(p)[0] - oklab(rgb[i])[0]).abs() < 1e-4, "lightness changed");
    }
    // every fringe pixel away from the picture's corners
    assert_eq!(changed.len(), 2 * h);
}

#[test]
fn bands_and_tiles_leave_no_seams() {
    // taller than a band and wider than a tile: every row is the same, so every row's change must be too
    let (w, h) = (300, 2 * BAND + 50);
    let (rgb, clip) = fringed(w, h);
    let changed = run(&model([-3.0, 3.0, 10.0]), &rgb, &clip, w, h);
    let mut rows: Vec<Vec<(usize, [f32; 3])>> = vec![Vec::new(); h];
    for &(i, p) in &changed {
        rows[i as usize / w].push((i as usize % w, p));
    }
    for (y, r) in rows.iter().enumerate() {
        assert_eq!(r.len(), 2, "row {y}");
        for (a, b) in r.iter().zip(&rows[0]) {
            assert_eq!(a.0, b.0);
            assert!(a.1.iter().zip(&b.1).all(|(u, v)| (u - v).abs() < 1e-5), "row {y}: {a:?} against {b:?}");
        }
    }
}

#[test]
fn small_pictures_are_padded() {
    let (rgb, clip) = fringed(150, 40);
    let changed = run(&model([-3.0, 3.0, 10.0]), &rgb, &clip, 150, 40);
    assert_eq!(changed.len(), 2 * 40);
}

#[test]
fn a_correction_stops_at_grey_and_does_not_make_green() {
    // a huge push against the pixel's own colour ends at grey, not past it
    let ab: [f32; 2] = [0.05, -0.05];
    let c = (ab[0] * ab[0] + ab[1] * ab[1]).sqrt();
    let new = compose(ab, c, 1.0, c, [-20.0, 20.0, 20.0]);
    assert!(new[0] >= -1e-6 && new[1] <= 1e-6, "{new:?}");
    // a push across into green on a grey-ish pixel stays under 0.01 of green chroma
    let ab = [0.0, 0.0];
    let new = compose(ab, 0.0, 1.0, 0.2, [-20.0, 5.0, 20.0]);
    let cn = (new[0] * new[0] + new[1] * new[1]).sqrt();
    assert!(cn <= 0.0101, "{new:?}");
    // flat areas are never touched
    assert_eq!(compose([0.03, 0.02], 0.036, 0.0, 0.05, [-5.0, -5.0, 20.0]), [0.03, 0.02]);
}

#[test]
fn oklab_round_trips() {
    // (colours inside Rec.709: the conversion clamps below it, as the model was trained)
    for p in [[0.18, 0.18, 0.18], [0.6, 0.3, 0.4], [0.3, 0.5, 0.25], [1.0, 1.0, 1.0]] {
        let back = lab_to_rgb(oklab(p));
        for (a, b) in p.iter().zip(&back) {
            assert!((a - b).abs() < 1e-4, "{p:?} → {back:?}");
        }
    }
}

#[test]
fn the_wrong_shape_of_network_is_refused() {
    let denoiser = crate::onnx::read_bytes(&crate::synthetic::smoothing_onnx()).unwrap();
    assert!(Model::new(&denoiser).is_err());
}

#[test]
fn cancelling_stops_the_run() {
    let (rgb, clip) = fringed(300, 280);
    let r = model([0.0, 0.0, 0.0]).correct(&rgb, &clip, 300, 280, &|_, _| {}, &AtomicBool::new(true));
    assert!(matches!(r, Err(Error::Cancelled)));
}

#[test]
fn the_clip_mask_marks_the_brightest_pixels() {
    let (rgb, _) = fringed(300, 20);
    let t = top(&rgb);
    assert!((t - 1.0).abs() < 1e-6);
    let mask = clip_mask(&rgb, 300, 20, t);
    assert!(mask[10 * 300 + 50] && !mask[10 * 300 + 200] && !mask[10 * 300 + 98]);
}
