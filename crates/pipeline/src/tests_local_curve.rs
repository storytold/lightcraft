//! Pixel tests of a mask's local Curve (`Mask::curve`).

use lcc_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, PointCurves};
use lcc_geom::Point;
use lcc_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, render};

const W: usize = 240;
const H: usize = 120;

/// A grey ramp, dark at the top and light at the bottom, the same in every column.
fn ramp() -> Rgb32f {
    Rgb32f::from_fn(W, H, |_, y| [0.01 + 0.8 * y as f32 / H as f32; 3])
}

/// Settings with one mask of `shape`, with the local Curve `curve` and Amount `amount`.
fn masked(shape: MaskShape, curve: PointCurves, amount: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    let mut m = Mask { id: 1, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], curve, ..Default::default() };
    m.adjust.amount = amount;
    s.masks.push(m);
    s
}

/// A hard mask over the left part (x < 0.45).
fn left() -> MaskShape {
    MaskShape::Linear { start: Point::new(0.45, 0.5), end: Point::new(0.46, 0.5) }
}

fn shot(src: &Rgb32f, s: &DevelopSettings) -> Rgba8 {
    render(src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image
}

fn px(img: &Rgba8, x: usize, y: usize) -> [i32; 3] {
    let p = img.data[y * W + x];
    [p[0] as i32, p[1] as i32, p[2] as i32]
}

fn pts(v: &[(f64, f64)]) -> Vec<Point> {
    v.iter().map(|&(x, y)| Point::new(x, y)).collect()
}

/// Brighter midtones.
fn lift() -> Vec<Point> {
    pts(&[(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)])
}

/// The inverse of [`lift`], as a curve through 33 of its points with the axes swapped.
fn unlift() -> Vec<Point> {
    let c = lcc_color::spline::MonotoneCurve::new(&[(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)]);
    (0..=32).map(|i| i as f64 / 32.0).map(|x| Point::new(c.eval(x), x)).collect()
}

#[test]
fn a_linear_local_curve_changes_nothing() {
    let src = ramp();
    let without = shot(&src, &masked(left(), PointCurves::default(), 100.0));
    let diagonal = pts(&[(0.0, 0.0), (0.3, 0.3), (1.0, 1.0)]);
    let linear = PointCurves { master: diagonal.clone(), red: diagonal.clone(), green: Vec::new(), blue: diagonal };
    assert_eq!(shot(&src, &masked(left(), linear, 100.0)).data, without.data);
    // and the mask without one renders as before local curves existed: as no mask at all, here
    assert_eq!(without.data, shot(&src, &DevelopSettings::default()).data);
}

#[test]
fn the_local_curve_works_inside_the_mask_only() {
    let src = ramp();
    let plain = shot(&src, &DevelopSettings::default());
    let lifted = shot(&src, &masked(left(), PointCurves { master: lift(), ..Default::default() }, 100.0));
    let y = H / 2;
    let (inside, before) = (px(&lifted, 40, y), px(&plain, 40, y));
    assert!(inside[1] > before[1] + 20, "midtones lifted inside: {inside:?} vs {before:?}");
    assert_eq!(inside[0], inside[1], "the RGB curve keeps a grey grey");
    assert_eq!(inside[1], inside[2]);
    for x in (W / 2..W).step_by(7) {
        for y in (0..H).step_by(5) {
            assert_eq!(px(&lifted, x, y), px(&plain, x, y), "outside the mask at ({x}, {y})");
        }
    }
    // the curve's end points stay where they are: the darkest and lightest rows move least
    let gain = |y: usize| px(&lifted, 40, y)[1] - px(&plain, 40, y)[1];
    assert!(gain(1) < gain(y) && gain(H - 2) < gain(y), "{} {} {}", gain(1), gain(y), gain(H - 2));
}

#[test]
fn a_colour_channel_curve_moves_that_channel_only() {
    let src = ramp();
    let plain = shot(&src, &DevelopSettings::default());
    let warm = shot(&src, &masked(left(), PointCurves { red: lift(), ..Default::default() }, 100.0));
    let (p, q) = (px(&warm, 40, H / 2), px(&plain, 40, H / 2));
    assert!(p[0] > q[0] + 20, "red lifted: {p:?} vs {q:?}");
    assert_eq!((p[1], p[2]), (q[1], q[2]), "green and blue as they were");
    // the RGB curve runs first, then the channel's: a red curve that undoes the RGB lift leaves red where it was
    let both = shot(&src, &masked(left(), PointCurves { master: lift(), red: unlift(), ..Default::default() }, 100.0));
    let b = px(&both, 40, H / 2);
    assert!((b[0] - q[0]).abs() <= 2, "red back where it was: {b:?} vs {q:?}");
    assert!(b[1] > q[1] + 20, "green lifted: {b:?} vs {q:?}");
}

#[test]
fn the_local_curve_follows_the_masks_weight() {
    let src = ramp();
    let plain = shot(&src, &DevelopSettings::default());
    let curve = PointCurves { master: lift(), ..Default::default() };
    let y = H / 2;
    let lift_at = |img: &Rgba8, x: usize| px(img, x, y)[1] - px(&plain, x, y)[1];
    // Amount scales it: half the Amount, half the change (in encoded values, to rounding)
    let full = shot(&src, &masked(left(), curve.clone(), 100.0));
    let half = shot(&src, &masked(left(), curve.clone(), 50.0));
    let (f, h) = (lift_at(&full, 40), lift_at(&half, 40));
    assert!((2 * h - f).abs() <= 2, "half Amount: {h} vs full {f}");
    // a feathered mask fades it: a linear gradient from full at x = 0.1 to none at x = 0.9
    let grad = MaskShape::Linear { start: Point::new(0.1, 0.5), end: Point::new(0.9, 0.5) };
    let faded = shot(&src, &masked(grad, curve.clone(), 100.0));
    let lifts: Vec<i32> = [10, 60, 120, 180, 230].iter().map(|&x| lift_at(&faded, x)).collect();
    assert!(lifts.windows(2).all(|w| w[0] >= w[1]), "fades across the gradient: {lifts:?}");
    assert_eq!(lifts[0], f, "full effect before the gradient starts");
    assert_eq!(lifts[4], 0, "none after it ends");
    assert!(lifts[2] > 0 && lifts[2] < f, "part of it in the middle: {lifts:?}");
    // an inverted mask moves the effect to the other side
    let mut inv = masked(left(), curve, 100.0);
    inv.masks[0].invert = true;
    let inverted = shot(&src, &inv);
    assert_eq!(lift_at(&inverted, 40), 0);
    assert_eq!(lift_at(&inverted, 200), f);
}

#[test]
fn two_masks_apply_their_curves_one_after_the_other() {
    let src = ramp();
    let plain = shot(&src, &DevelopSettings::default());
    let up = PointCurves { master: lift(), ..Default::default() };
    let down = PointCurves { master: unlift(), ..Default::default() };
    let mut s = masked(left(), up, 100.0);
    let mut second = masked(left(), down, 100.0).masks.remove(0);
    second.id = 2;
    s.masks.push(second);
    // the second mask's curve is the first one's inverse here: together they cancel, to rounding
    let both = shot(&src, &s);
    for y in (10..H - 10).step_by(10) {
        let (b, p) = (px(&both, 40, y), px(&plain, 40, y));
        assert!((b[1] - p[1]).abs() <= 2, "row {y}: {b:?} vs {p:?}");
    }
}
