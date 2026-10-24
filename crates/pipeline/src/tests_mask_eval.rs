//! Evaluation of the AI masks without a model (Sky, Subject, Landscape) against reference masks:
//! the procedural demo scenes (`lightcraft-scenes`) know what each pixel shows ([`lightcraft_scenes::Label`]),
//! so every scene of the demo library is a labelled photo. Scores are soft IoU
//! (Σ min / Σ max of the mask and the reference share) per scene; the tests print the table
//! (`cargo test -p lightcraft-pipeline mask_eval -- --nocapture`) and hold the means to a floor, so a
//! change that makes a mask worse fails. Numbers are recorded in docs/ai-masks.md.

use lightcraft_develop::{DevelopSettings, MaskShape};
use lightcraft_raster::Plane;
use lightcraft_scenes::Label;

use crate::geometry::Frame;
use crate::masks::{Mattes, shape_alpha};

const EDGE: usize = 384;

fn iou(a: &Plane, truth: &[[f32; 4]], label: Label) -> Option<f64> {
    let (mut inter, mut uni) = (0.0f64, 0.0f64);
    for (m, t) in a.data.iter().zip(truth) {
        let t = t[label as usize];
        inter += m.min(t) as f64;
        uni += m.max(t) as f64;
    }
    let total: f64 = truth.iter().map(|t| t[label as usize] as f64).sum();
    // scenes without any of the label don't score it
    (total > 0.002 * truth.len() as f64).then(|| inter / uni.max(1e-9))
}

/// Mean IoU of `shape` over the demo scenes that show `label`, printing each scene's score:
/// with the photo analysed ([`crate::auto_mask`], `auto`) or with the per-pixel fallback.
fn evaluate(shape: &MaskShape, label: Label, auto: bool) -> f64 {
    let mut scores = Vec::new();
    for scene in lightcraft_scenes::demo_library() {
        let img = scene.render_fit(EDGE);
        let (w, h) = (img.width, img.height);
        let truth = scene.labels(w, h);
        let log_l = img.map(crate::local::log_lum);
        let frame = Frame::new(w, h, &DevelopSettings::default(), true);
        let mut mattes = Mattes::default();
        if auto {
            let proxy = scene.render_fit(crate::auto_mask::EDGE);
            mattes.auto = Some(std::sync::Arc::new(crate::auto_mask::analyze(&proxy)));
        }
        let a = shape_alpha(shape, &frame, w, h, &img, &log_l, 0.0, Some(&mattes));
        if let Ok(dir) = std::env::var("MASK_EVAL_DUMP") {
            let mut buf = format!("P6\n{} {}\n255\n", w * 3, h).into_bytes();
            for y in 0..h {
                for x in 0..w {
                    let c = img.data[y * w + x].map(|v| ((v / (1.0 + v)).powf(0.45) * 255.0) as u8);
                    buf.extend_from_slice(&c);
                }
                for x in 0..w {
                    buf.extend_from_slice(&[(a.data[y * w + x] * 255.0) as u8; 3]);
                }
                for x in 0..w {
                    buf.extend_from_slice(&[(truth[y * w + x][label as usize] * 255.0) as u8; 3]);
                }
            }
            let _ = std::fs::write(format!("{dir}/{label:?}-{:?}-{}-{auto}.ppm", scene.kind, scene.seed), buf);
        }
        if let Some(s) = iou(&a, &truth, label) {
            println!("{label:?} {:?} seed {}: IoU {s:.3}", scene.kind, scene.seed);
            scores.push(s);
        }
    }
    let mean = scores.iter().sum::<f64>() / scores.len().max(1) as f64;
    println!("{label:?} (auto: {auto}): mean IoU {mean:.3} over {} scenes", scores.len());
    mean
}

/// Before (per-pixel fallback) and after (whole-photo analysis); the floors sit just under the
/// measured means.
#[test]
fn mask_eval_sky() {
    let before = evaluate(&MaskShape::Sky, Label::Sky, false);
    let after = evaluate(&MaskShape::Sky, Label::Sky, true);
    assert!(after > before && after > 0.77, "{before} → {after}");
}

#[test]
fn mask_eval_subject() {
    let before = evaluate(&MaskShape::Subject, Label::Subject, false);
    let after = evaluate(&MaskShape::Subject, Label::Subject, true);
    assert!(after > before && after > 0.85, "{before} → {after}");
}

#[test]
fn mask_eval_water() {
    let before = evaluate(&MaskShape::Landscape { class: "water".into() }, Label::Water, false);
    let after = evaluate(&MaskShape::Landscape { class: "water".into() }, Label::Water, true);
    assert!(after > before && after > 0.3, "{before} → {after}");
}
