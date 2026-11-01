//! Moving a mask's coordinates between frames: [`attach`] puts an old mask's coordinates (on the
//! corrected frame) on the photo before the warp ([`lightcraft_develop::Mask::on_photo`]), and
//! [`turn`] follows a quarter turn or flip of the photo (Rotate Left / Right). Gradients, brush
//! strokes and clicks move point by point; stored AI segmentations are resampled.

use lightcraft_develop::{Mask, MaskShape, SegMask};
use lightcraft_geom::{Orientation, Point};

use crate::geometry::Frame;
use crate::masks::SegGrid;

/// Put mask `m`, drawn on the corrected (transformed) image of `frame`, on the photo before the
/// warp: it then renders where it did under `frame` and stays on that content whatever lens
/// corrections, Upright or Transform do later. A mask already on the photo is left alone.
pub fn attach(m: &mut Mask, frame: &Frame) {
    if m.on_photo {
        return;
    }
    m.on_photo = true;
    let f = frame.for_mask(true);
    if f.mask_warp().is_none() {
        // without a warp both are the same coordinates
        return;
    }
    let fwd = |p: Point| f.norm_to_mask(p);
    let back = |p: Point| f.mask_to_norm(p);
    remap(m, &fwd, &back, 0.0);
}

/// Follow `turn` (the change of the user orientation, e.g. a quarter turn clockwise): mask
/// coordinates are on the oriented photo, so they turn with it. Radial angles turn by the same
/// quarter turns; sizes (long-edge units) don't change.
pub fn turn(m: &mut Mask, turn: Orientation) {
    if turn == Orientation::Normal {
        return;
    }
    let fwd = |p: Point| {
        let (x, y) = turn.map(p.x, p.y, 1.0, 1.0);
        Point::new(x, y)
    };
    let inv = turn.inverse();
    let back = |p: Point| {
        let (x, y) = inv.map(p.x, p.y, 1.0, 1.0);
        Point::new(x, y)
    };
    let (flip, quarters) = turn.to_parts();
    // a radial's angle is clockwise on screen: a quarter turn clockwise adds 90°; a mirror negates it first
    let delta = 90.0 * quarters as f64;
    remap_turn(m, &fwd, &back, flip, delta);
}

fn remap_turn(m: &mut Mask, fwd: &dyn Fn(Point) -> Point, back: &dyn Fn(Point) -> Point, flip: bool, delta: f64) {
    if flip {
        for c in &mut m.components {
            if let MaskShape::Radial { angle, .. } = &mut c.shape {
                *angle = -*angle;
            }
        }
    }
    remap(m, fwd, back, delta);
}

/// Move every place `m` stores: `fwd` maps an old point to the new frame, `back` a new point to
/// the old one (for resampling segmentations); radial angles change by `delta` degrees.
fn remap(m: &mut Mask, fwd: &dyn Fn(Point) -> Point, back: &dyn Fn(Point) -> Point, delta: f64) {
    for c in &mut m.components {
        match &mut c.shape {
            MaskShape::Linear { start, end } => {
                *start = fwd(*start);
                *end = fwd(*end);
            }
            MaskShape::Radial { center, angle, .. } => {
                *center = fwd(*center);
                if delta != 0.0 {
                    // (kept within −180..180°)
                    *angle = (*angle + delta + 180.0).rem_euclid(360.0) - 180.0;
                }
            }
            MaskShape::Brush { strokes } => {
                for s in strokes {
                    s.points.iter_mut().for_each(|p| *p = fwd(*p));
                }
            }
            MaskShape::Object { hint, exclude, seg, detail, .. } => {
                hint.iter_mut().chain(exclude.iter_mut()).for_each(|p| *p = fwd(*p));
                remap_seg(seg, fwd, back);
                detail.iter_mut().for_each(|d| *d = moved(d, fwd, back).unwrap_or_else(|| d.clone()));
            }
            MaskShape::Prompt { seg, detail, .. } => {
                remap_seg(seg, fwd, back);
                detail.iter_mut().for_each(|d| *d = moved(d, fwd, back).unwrap_or_else(|| d.clone()));
            }
            // computed from the photo at each render (its mattes follow the warp), or colours,
            // tones and depths, which have no place (a colour range's samples are colours)
            MaskShape::Subject
            | MaskShape::Sky
            | MaskShape::Background
            | MaskShape::People { .. }
            | MaskShape::Landscape { .. }
            | MaskShape::ColorRange { .. }
            | MaskShape::LuminanceRange { .. }
            | MaskShape::DepthRange { .. } => {}
        }
    }
}

fn remap_seg(seg: &mut Option<SegMask>, fwd: &dyn Fn(Point) -> Point, back: &dyn Fn(Point) -> Point) {
    if let Some(s) = seg.as_ref()
        && let Some(m) = moved(s, fwd, back)
    {
        *seg = Some(m);
    }
}

/// The box around `r` mapped by `f` (its corners and edge midpoints), inside 0..1.
fn map_rect(r: [f64; 4], f: &dyn Fn(Point) -> Point) -> [f64; 4] {
    let (mx, my) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
    let pts =
        [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3]), (mx, r[1]), (mx, r[3]), (r[0], my), (r[2], my)].map(|(x, y)| f(Point::new(x, y)));
    let finite = pts.iter().filter(|p| p.x.is_finite() && p.y.is_finite());
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in finite {
        (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
    }
    if x0 > x1 || y0 > y1 {
        return r;
    }
    [x0.clamp(0.0, 1.0), y0.clamp(0.0, 1.0), x1.clamp(0.0, 1.0), y1.clamp(0.0, 1.0)]
}

/// Segmentation `s` resampled onto the new frame: a grid of the same side over the mapped box
/// (the whole image for a whole-image one), each cell taking the old logit at its centre mapped
/// back. `None` when the data is damaged (it is kept as it is).
fn moved(s: &SegMask, fwd: &dyn Fn(Point) -> Point, back: &dyn Fn(Point) -> Point) -> Option<SegMask> {
    let old = SegGrid::new(s)?;
    let side = s.side as usize;
    let rect = match s.rect {
        None => None,
        Some(r) => Some(map_rect(r, fwd)).filter(|b| b[2] > b[0] && b[3] > b[1]),
    };
    let b = rect.unwrap_or([0.0, 0.0, 1.0, 1.0]);
    let logits: Vec<f32> = (0..side * side)
        .map(|i| {
            let (x, y) = ((i % side) as f64 + 0.5, (i / side) as f64 + 0.5);
            let n = Point::new(b[0] + x / side as f64 * (b[2] - b[0]), b[1] + y / side as f64 * (b[3] - b[1]));
            old.logit(back(n))
        })
        .collect();
    Some(match rect {
        Some(r) => SegMask::from_logits_in(side, &logits, r),
        None => SegMask::from_logits(side, &logits),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::{BrushStroke, DevelopSettings, MaskComponent, MaskOp};

    fn comp(shape: MaskShape) -> MaskComponent {
        MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }
    }

    fn mask(shape: MaskShape) -> Mask {
        Mask { components: vec![comp(shape)], ..Default::default() }
    }

    fn seg_at(p: Point) -> SegMask {
        const SIDE: usize = 32;
        let l: Vec<f32> = (0..SIDE * SIDE)
            .map(|i| {
                let (x, y) = (((i % SIDE) as f64 + 0.5) / SIDE as f64, ((i / SIDE) as f64 + 0.5) / SIDE as f64);
                if (x - p.x).hypot(y - p.y) < 0.08 { 8.0 } else { -8.0 }
            })
            .collect();
        SegMask::from_logits(SIDE, &l)
    }

    fn object_at(p: Point) -> MaskShape {
        MaskShape::Object { hint: vec![p], exclude: Vec::new(), seg: Some(seg_at(p)), detail: Vec::new(), edge: 0.0 }
    }

    /// The centre of what `s` selects (sampled on a fine grid).
    fn centre_of(s: &SegMask) -> Point {
        let g = SegGrid::new(s).unwrap();
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for i in 0..200 * 200 {
            let p = Point::new(((i % 200) as f64 + 0.5) / 200.0, ((i / 200) as f64 + 0.5) / 200.0);
            if g.contains(p) && g.logit(p) > 0.0 {
                (sx, sy, n) = (sx + p.x, sy + p.y, n + 1.0);
            }
        }
        assert!(n > 0.0, "nothing selected");
        Point::new(sx / n, sy / n)
    }

    fn seg_of(m: &Mask, i: usize) -> &SegMask {
        match &m.components[i].shape {
            MaskShape::Object { seg: Some(s), .. } => s,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_quarter_turn_turns_every_place_a_mask_keeps() {
        let p = Point::new(0.2, 0.3);
        let mut m = mask(MaskShape::Radial { center: p, rx: 0.1, ry: 0.05, angle: 10.0, feather: 50.0, invert: false });
        m.components.push(comp(MaskShape::Brush { strokes: vec![BrushStroke { points: vec![p], ..Default::default() }] }));
        m.components.push(comp(object_at(p)));
        turn(&mut m, Orientation::Rotate90);
        // a quarter turn clockwise: (x, y) → (1 − y, x)
        let want = Point::new(0.7, 0.2);
        let MaskShape::Radial { center, angle, .. } = &m.components[0].shape else { panic!() };
        assert!((center.x - want.x).abs() < 1e-9 && (center.y - want.y).abs() < 1e-9, "{center:?}");
        assert_eq!(*angle, 100.0);
        let MaskShape::Brush { strokes } = &m.components[1].shape else { panic!() };
        assert!((strokes[0].points[0].x - want.x).abs() < 1e-9);
        let MaskShape::Object { hint, .. } = &m.components[2].shape else { panic!() };
        assert!((hint[0].x - want.x).abs() < 1e-9 && (hint[0].y - want.y).abs() < 1e-9, "{hint:?}");
        let c = centre_of(seg_of(&m, 2));
        assert!((c.x - want.x).abs() < 0.04 && (c.y - want.y).abs() < 0.04, "{c:?}");
        // and back
        turn(&mut m, Orientation::Rotate270);
        let MaskShape::Radial { center, angle, .. } = &m.components[0].shape else { panic!() };
        assert!((center.x - p.x).abs() < 1e-9 && (center.y - p.y).abs() < 1e-9 && *angle == 10.0);
    }

    #[test]
    fn attaching_an_old_mask_keeps_where_it_renders() {
        let mut s = DevelopSettings::default();
        s.geometry.vertical = 30.0;
        s.geometry.rotate = 5.0;
        let frame = Frame::new(300, 200, &s, false);
        let p = Point::new(0.3, 0.4);
        let mut m = mask(MaskShape::Linear { start: p, end: Point::new(0.6, 0.4) });
        m.components.push(comp(object_at(p)));
        m.on_photo = false;
        attach(&mut m, &frame);
        assert!(m.on_photo);
        let on = frame.for_mask(true);
        let MaskShape::Linear { start, .. } = &m.components[0].shape else { panic!() };
        let back = on.mask_to_norm(*start);
        assert!((back.x - p.x).abs() < 1e-6 && (back.y - p.y).abs() < 1e-6, "{back:?}");
        assert!((start.x - p.x).hypot(start.y - p.y) > 0.005, "the warp moved it on the photo: {start:?}");
        let c = on.mask_to_norm(centre_of(seg_of(&m, 1)));
        assert!((c.x - p.x).abs() < 0.04 && (c.y - p.y).abs() < 0.04, "{c:?}");
        // attaching again changes nothing
        let before = m.clone();
        attach(&mut m, &frame);
        assert_eq!(m, before);
    }

    #[test]
    fn damaged_or_hostile_masks_move_without_a_panic() {
        let mut m =
            mask(MaskShape::Radial { center: Point::new(f64::NAN, f64::INFINITY), rx: 0.1, ry: 0.1, angle: f64::NAN, feather: 0.0, invert: false });
        m.components.push(comp(MaskShape::Object {
            hint: vec![Point::new(1e300, -1e300)],
            exclude: Vec::new(),
            seg: Some(SegMask { side: 1024, data: "not base64".into(), rect: Some([f64::NAN, 0.0, 2.0, -1.0]) }),
            detail: vec![SegMask { side: 0, data: String::new(), rect: None }],
            edge: 0.0,
        }));
        m.on_photo = false;
        let mut s = DevelopSettings::default();
        s.geometry.vertical = 80.0;
        s.optics.distortion = 100.0;
        attach(&mut m, &Frame::new(300, 200, &s, false));
        for o in [Orientation::Rotate90, Orientation::FlipH, Orientation::Rotate180] {
            turn(&mut m, o);
        }
        assert!(m.on_photo);
    }
}
