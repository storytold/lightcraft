//! Compatibility with main 76b53778: hidden and empty masks still select the tone
//! base consumed by Clarity, even when no visible Highlights or Shadows are set.
use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape, Process};
use lightcraft_geom::Point;
use lightcraft_pipeline::{PixelWindow, RenderRequest, SourceInfo, plan, render};
use lightcraft_raster::Rgb32f;

fn source() -> Rgb32f {
    Rgb32f::from_fn(600, 400, |x, y| {
        let l = 0.003 + (((x * 17 + y * 23) % 103) as f32 / 102.0).powi(2);
        [l * 1.1, l, l * 0.8]
    })
}

fn mask() -> Mask {
    Mask {
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.1, 0.2), end: Point::new(0.9, 0.8) },
        }],
        ..Default::default()
    }
}

fn settings(process: Process, empty: bool, local_clarity: bool) -> DevelopSettings {
    let mut s = DevelopSettings { process: process.version(), ..Default::default() };
    let mut m = mask();
    m.visible = empty;
    if empty {
        m.components.clear();
    }
    m.adjust = LocalAdjustments { highlights: -80.0, shadows: 80.0, ..Default::default() };
    s.masks.push(m);
    if local_clarity {
        let mut m = mask();
        m.id = 2;
        m.adjust.clarity = 100.0;
        s.masks.push(m);
    } else {
        s.effects.clarity = 100.0;
    }
    s
}

#[test]
fn full_clarity_with_hidden_or_empty_tone_masks_matches_main() {
    let src = source();
    let info = SourceInfo::default();
    let req = RenderRequest::fit(1200, 800);
    for process in [Process::V1, Process::V2] {
        for local_clarity in [false, true] {
            // Main selects the tone base for a visible tone mask even when its
            // amount is zero. It changes no pixels directly. Compute that
            // reference on this platform instead of hashing saved RGBA bytes.
            let mut reference = settings(process, false, local_clarity);
            reference.masks[0].visible = true;
            reference.masks[0].adjust.amount = 0.0;
            let expected = render(&src, &info, &reference, &req).unwrap().image;
            let mut no_tone = reference.clone();
            no_tone.masks.remove(0);
            let without_base = render(&src, &info, &no_tone, &req).unwrap().image;
            assert_ne!(expected.data, without_base.data, "the fixture must distinguish Clarity's tone base");
            for empty in [false, true] {
                let s = settings(process, empty, local_clarity);
                let full = render(&src, &info, &s, &req).unwrap().image;
                assert_eq!(full.data, expected.data, "{process:?} local_clarity={local_clarity} empty={empty}");
            }
        }
    }
}

#[test]
fn clarity_windows_include_hidden_or_empty_tone_base_support() {
    let src = source();
    let info = SourceInfo::default();
    // These radii use the full-resolution guided filter. This isolates support
    // from subsampling, which has its own full-frame-grid regressions.
    let req = RenderRequest::fit(360, 240);
    let at = PixelWindow { x: 121, y: 77, w: 121, h: 83 };
    let window_req = RenderRequest { window: Some(at), ..req };
    for process in [Process::V1, Process::V2] {
        for local_clarity in [false, true] {
            for empty in [false, true] {
                let s = settings(process, empty, local_clarity);
                let full = render(&src, &info, &s, &req).unwrap().image.crop(at.x, at.y, at.w, at.h);
                let window = render(&src, &info, &s, &window_req).unwrap().image;
                let max = full.data.iter().flatten().zip(window.data.iter().flatten()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                assert!(max <= 1, "{process:?} local_clarity={local_clarity} empty={empty}: max={max}");
                let p = plan(&src, &info, &s, &window_req);
                let mut active_tone = s.clone();
                active_tone.light.highlights = -80.0;
                let tone_plan = plan(&src, &info, &active_tone, &window_req);
                assert_eq!((p.w, p.h, p.keep), (tone_plan.w, tone_plan.h, tone_plan.keep), "Clarity consumes the same tone base");
                let mut no_tone = s.clone();
                no_tone.masks.remove(0);
                let clarity_plan = plan(&src, &info, &no_tone, &window_req);
                assert!(p.w > clarity_plan.w, "tone-base support exceeds Clarity's own support");
            }
        }
    }
}
