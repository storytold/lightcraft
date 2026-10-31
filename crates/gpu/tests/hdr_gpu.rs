//! The float HDR rendition (a gain map JPEG's) on the GPU matches the CPU's `finish_deep`: before,
//! every HDR export rendered it on the CPU however good the graphics card. Compared in linear
//! light (relative to SDR white, up to the peak) and in 8-bit sRGB of the same samples. Skips
//! without a GPU adapter.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, VignetteStyle};
use lightcraft_geom::Point;
use lightcraft_pipeline::{DeepSamples, OutputDepth, OutputSpace, RenderRequest, SourceInfo, StageCache, render};

fn gpu() -> bool {
    let ok = lightcraft_gpu::available();
    if !ok {
        let why = lightcraft_gpu::unavailable_reason().unwrap_or_default();
        assert!(!why.contains("kernels failed to build"), "{why}");
        lightcraft_testkit::skip!(Gpu, "no GPU adapter");
    }
    ok
}

fn hdr_request(w: usize, h: usize, space: OutputSpace) -> RenderRequest {
    RenderRequest { depth: OutputDepth::F32Hdr, space, ..RenderRequest::fit(w, h) }
}

/// (mean, max) difference of two float renditions in linear light, and the brightest value.
fn deep_diff(a: &DeepSamples, b: &DeepSamples) -> (f64, f32, f32) {
    let (DeepSamples::F32(a), DeepSamples::F32(b)) = (a, b) else { panic!("float samples expected") };
    assert_eq!(a.len(), b.len());
    let (mut sum, mut max, mut top) = (0f64, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        // relative above white, absolute below (an SDR LSB is about 0.002 in linear light near white)
        let d = (x - y).abs() / x.abs().max(1.0);
        sum += d as f64;
        max = max.max(d);
        top = top.max(*x);
    }
    (sum / a.len().max(1) as f64, max, top)
}

fn check(name: &str, src: &Arc<lightcraft_raster::Rgb32f>, s: &DevelopSettings, req: &RenderRequest) {
    let info = SourceInfo { raw: true, ..Default::default() };
    let cpu = render(src, &info, s, req);
    let gpu = lightcraft_gpu::try_render(src, &info, s, req, None).unwrap_or_else(|e| panic!("{name}: the GPU did not render it: {e:?}"));
    let (c, g) = (cpu.deep.as_ref().expect("cpu deep"), gpu.deep.as_ref().expect("gpu deep"));
    assert_eq!((c.width, c.height, c.space), (g.width, g.height, g.space), "{name}");
    let (mean, max, top) = deep_diff(&c.samples, &g.samples);
    // the 8-bit image made from the samples, as the CPU's
    let (mut lsb, mut lsb_max) = (0u64, 0u8);
    for (p, q) in cpu.image.data.iter().zip(&gpu.image.data) {
        for k in 0..3 {
            let d = p[k].abs_diff(q[k]);
            lsb += d as u64;
            lsb_max = lsb_max.max(d);
        }
    }
    let lsb = lsb as f64 / (cpu.image.data.len() * 3).max(1) as f64;
    eprintln!("{name:<28} {}x{}  linear mean {mean:.5} max {max:.4} (brightest {top:.2})  8-bit mean {lsb:.4} max {lsb_max}", c.width, c.height);
    assert!(top > 1.05, "{name}: nothing above SDR white to compare");
    assert!(mean < 2e-3 && max < 0.03, "{name}: linear mean {mean:.5}, max {max:.4}");
    assert!(lsb < 0.5 && lsb_max <= 3, "{name}: 8-bit mean {lsb:.4}, max {lsb_max}");
}

fn hdr_edit() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.hdr.enabled = true;
    s.hdr.max_ev = 3.0;
    s.light.exposure = 0.6;
    s.light.highlights = -30.0;
    s.light.shadows = 20.0;
    s.effects.clarity = 15.0;
    s.detail.nr_luminance = 20.0;
    s.detail.sharpen_amount = 30.0;
    s
}

#[test]
fn the_hdr_rendition_on_the_gpu_matches_the_cpu() {
    if !gpu() {
        return;
    }
    let (w, h) = (900, 600);
    // the demo's ocean sunset: a sun far above SDR white
    let src = Arc::new(lightcraft_scenes::demo_library().into_iter().find(|s| s.name == "Golden horizon").expect("sunset").render(w, h));
    let s = hdr_edit();
    check("hdr edit", &src, &s, &hdr_request(w, h, OutputSpace::Srgb));
    check("display p3", &src, &s, &hdr_request(w, h, OutputSpace::DisplayP3));
    check("rec 2020", &src, &s, &hdr_request(w, h, OutputSpace::Rec2020));
    // a tone curve and a vignette lightening (both continue above SDR white), a mask
    let mut t = hdr_edit();
    t.curve.master = vec![Point::new(0.0, 0.0), Point::new(0.25, 0.2), Point::new(0.75, 0.8), Point::new(1.0, 1.0)];
    t.curve.highlights = 20.0;
    t.vignette.amount = 30.0;
    t.vignette.style = VignetteStyle::HighlightPriority;
    t.masks.push(Mask {
        id: 1,
        visible: true,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Radial { center: Point::new(0.5, 0.4), rx: 0.3, ry: 0.25, angle: 0.0, feather: 50.0, invert: false },
        }],
        ..Default::default()
    });
    t.masks[0].adjust.exposure = 0.5;
    check("curves, vignette, a mask", &src, &t, &hdr_request(w, h, OutputSpace::DisplayP3));
}

/// An export's two renditions: the SDR one, then the HDR one from the same view's stages (the
/// device keeps them), the same as the HDR one alone.
#[test]
fn the_hdr_rendition_from_the_sdr_ones_stages_matches() {
    if !gpu() {
        return;
    }
    let (w, h) = (900, 600);
    let src = Arc::new(lightcraft_scenes::demo_library().into_iter().find(|s| s.name == "Golden horizon").expect("sunset").render(w, h));
    let info = SourceInfo { raw: true, ..Default::default() };
    let s = hdr_edit();
    let hdr = hdr_request(w, h, OutputSpace::DisplayP3);
    let sdr = RenderRequest { depth: OutputDepth::U8, ..hdr };
    let alone = lightcraft_gpu::try_render(&src, &info, &s, &hdr, None).expect("gpu").deep.expect("deep");
    let stages = StageCache::default();
    lightcraft_gpu::try_render(&src, &info, &s, &sdr, Some(&stages)).expect("sdr on the gpu");
    let after = lightcraft_gpu::try_render(&src, &info, &s, &hdr, Some(&stages)).expect("gpu").deep.expect("deep");
    let (mean, max, _) = deep_diff(&alone.samples, &after.samples);
    assert!(mean < 1e-6 && max < 1e-5, "mean {mean}, max {max}");
}
