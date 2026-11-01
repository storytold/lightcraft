//! On the GPU, masks stay on the image content under every geometry change:
//! a mask drawn around a mark still brightens that mark after Straighten, a crop, a flip, a quarter
//! turn or a lens / perspective warp, rendered fresh or through the view's stage cache. (The CPU
//! side is `lightcraft-pipeline`'s `tests_mask_geometry`.)

use std::sync::Arc;

use lightcraft_develop::{BrushStroke, DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape, SegMask};
use lightcraft_geom::{CropGeometry, Point, Rect};
use lightcraft_pipeline::{RenderRequest, SourceInfo, StageCache};
use lightcraft_raster::{Rgb32f, Rgba8};

const W: usize = 240;
const H: usize = 160;
const MARK: (f64, f64) = (0.3, 0.32);

fn gpu() -> bool {
    let ok = lightcraft_gpu::available();
    if !ok {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter");
    }
    ok
}

fn marked(mark: bool) -> Rgb32f {
    let (mx, my) = (MARK.0 * W as f64, MARK.1 * H as f64);
    Rgb32f::from_fn(W, H, |x, y| {
        let d = ((x as f64 + 0.5 - mx).powi(2) + (y as f64 + 0.5 - my).powi(2)).sqrt();
        if d < 2.5 && mark { [0.9; 3] } else { [0.05; 3] }
    })
}

fn with_mask(shape: MaskShape) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.masks.push(Mask {
        id: 1,
        components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
        adjust: LocalAdjustments { exposure: 2.0, ..Default::default() },
        ..Default::default()
    });
    s
}

fn shapes() -> Vec<(&'static str, MaskShape)> {
    let l = W.max(H) as f64;
    const SIDE: usize = 64;
    let logits: Vec<f32> = (0..SIDE * SIDE)
        .map(|i| {
            let (x, y) = ((i % SIDE) as f64 + 0.5, (i / SIDE) as f64 + 0.5);
            let (dx, dy) = ((x / SIDE as f64 - MARK.0) * W as f64, (y / SIDE as f64 - MARK.1) * H as f64);
            if (dx * dx + dy * dy).sqrt() < 10.0 { 8.0 } else { -8.0 }
        })
        .collect();
    vec![
        ("radial", MaskShape::Radial { center: Point::new(MARK.0, MARK.1), rx: 12.0 / l, ry: 12.0 / l, angle: 0.0, feather: 40.0, invert: false }),
        (
            "brush",
            MaskShape::Brush {
                strokes: vec![BrushStroke {
                    points: vec![Point::new(MARK.0 - 0.005, MARK.1), Point::new(MARK.0 + 0.005, MARK.1)],
                    size: 8.0 / l,
                    ..Default::default()
                }],
            },
        ),
        (
            "object",
            MaskShape::Object {
                hint: vec![Point::new(MARK.0, MARK.1)],
                exclude: Vec::new(),
                seg: Some(SegMask::from_logits(SIDE, &logits)),
                detail: Vec::new(),
                edge: 0.0,
            },
        ),
    ]
}

fn geometries() -> Vec<(&'static str, Box<dyn Fn(&mut DevelopSettings)>)> {
    vec![
        ("straighten", Box::new(|s: &mut DevelopSettings| s.crop.geometry = CropGeometry { rect: Rect::new(0.08, 0.1, 0.92, 0.9), angle: 7.0 })),
        ("crop", Box::new(|s: &mut DevelopSettings| s.crop.geometry.rect = Rect::new(0.1, 0.15, 0.7, 0.8))),
        ("flip", Box::new(|s: &mut DevelopSettings| s.crop.flip_h = true)),
        (
            "perspective",
            Box::new(|s: &mut DevelopSettings| {
                s.geometry.vertical = 25.0;
                s.geometry.rotate = 4.0;
            }),
        ),
        ("distortion", Box::new(|s: &mut DevelopSettings| s.optics.distortion = 40.0)),
    ]
}

fn luma(img: &Rgba8, i: usize) -> f64 {
    let p = img.data[i];
    (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0
}

fn centroid(w: usize, weights: impl Iterator<Item = f64>) -> (f64, f64) {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for (i, v) in weights.enumerate() {
        sx += v * ((i % w) as f64 + 0.5);
        sy += v * ((i / w) as f64 + 0.5);
        n += v;
    }
    assert!(n > 0.0, "nothing found");
    (sx / n, sy / n)
}

fn on_gpu(src: &Arc<Rgb32f>, s: &DevelopSettings, cache: Option<&StageCache>) -> Rgba8 {
    let req = RenderRequest::fit(W, H);
    lightcraft_gpu::try_render(src, &SourceInfo::default(), s, &req, cache).map(|r| r.image).unwrap_or_else(|e| panic!("GPU render failed: {e:?}"))
}

#[test]
fn masks_stay_on_the_content_under_every_geometry_change_on_the_gpu() {
    if !gpu() {
        return;
    }
    let src = Arc::new(marked(true));
    for (name, shape) in shapes() {
        for (what, change) in geometries() {
            let mut s = with_mask(shape.clone());
            let cache = StageCache::default();
            on_gpu(&src, &s, Some(&cache));
            change(&mut s);
            let unmasked = DevelopSettings { masks: Vec::new(), ..s.clone() };
            let plain = on_gpu(&src, &unmasked, None);
            // (where the marked photo differs from an unmarked one: a warp leaves light blank canvas)
            let bare = on_gpu(&Arc::new(marked(false)), &unmasked, None);
            let mark = centroid(plain.width, (0..plain.data.len()).map(|i| (luma(&plain, i) - luma(&bare, i)).max(0.0)));
            for (how, m) in [("fresh", on_gpu(&src, &s, None)), ("cached", on_gpu(&src, &s, Some(&cache)))] {
                let at = centroid(m.width, (0..m.data.len()).map(|i| (luma(&m, i) - luma(&plain, i)).max(0.0)));
                let d = ((at.0 - mark.0).powi(2) + (at.1 - mark.1).powi(2)).sqrt();
                assert!(d < 2.0, "{name} after {what} ({how}): the mask's effect is at {at:?}, the mark at {mark:?}");
            }
        }
    }
}
