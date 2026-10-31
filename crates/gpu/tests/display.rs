//! Renders handed to the window as textures (`render_display` on an adopted device): the texture
//! holds exactly the pixels a readback render returns, the histogram matches, and what can't be a
//! texture (overlays, a window grown by spots) comes back as pixels. Own process: adopting a
//! device is once per process. Skips (passes with a note) without an adapter.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Spot};
use lightcraft_geom::Point;
use lightcraft_gpu::{Output, wgpu};
use lightcraft_pipeline::{Overlay, PixelWindow, Quality, RenderRequest, SourceInfo, StageCache};

/// A device made the way eframe makes the window's (adapter limits, the window's backends).
fn window_device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = lightcraft_gpu::backend::window_backends();
    desc.backend_options = lightcraft_gpu::backend::backend_options();
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("test window"),
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .ok()?;
    Some((device, queue, info))
}

#[test]
fn display_renders_match_readback_renders() {
    // switched off for the process (`LIGHTCRAFT_GPU=0`): there is no device to adopt
    if let Some(reason) = lightcraft_gpu::env_off_reason() {
        lightcraft_testkit::skip!(Gpu, "{reason}");
        return;
    }
    let Some((device, queue, info)) = window_device() else {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter");
        return;
    };
    if info.device_type == wgpu::DeviceType::Cpu {
        lightcraft_testkit::skip!(Gpu, "software adapter");
        return;
    }
    let raw = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.5;
    s.light.highlights = -40.0;
    s.effects.clarity = 20.0;
    s.detail.nr_luminance = 20.0;
    s.grain.amount = 20.0;
    if let Err(e) = lightcraft_gpu::adopt_window_device(device.clone(), queue.clone(), info) {
        // another card edits (`LIGHTCRAFT_GPU_ADAPTER` names one the screen isn't on, on a machine with
        // two): the window's device is declined by design, and display renders come back as pixels
        assert!(e.contains("isn't the screen's"), "adopted: {e}");
        assert!(!lightcraft_gpu::on_window_device());
        let src = Arc::new(lightcraft_scenes::demo_library()[0].render(640, 427));
        let req = RenderRequest::fit(640, 427);
        let pixels = lightcraft_gpu::render(&src, &raw, &s, &req, None).expect("gpu render");
        match lightcraft_gpu::render_display(&src, &raw, &s, &req, None) {
            Some(Output::Image(r)) => assert!(r.image.data == pixels.image.data, "the pixels are the readback's"),
            _ => panic!("the editing card isn't the screen's: expected pixels"),
        }
        lightcraft_testkit::skip!(Gpu, "the editing card isn't the screen's ({e}): texture path not tested");
        return;
    }
    assert!(lightcraft_gpu::on_window_device());
    // widths that are and aren't a multiple of 64 texels (row padding), draft and full quality
    for (w, h, quality) in [(640, 427, Quality::Full), (1000, 667, Quality::Draft), (1024, 683, Quality::Full)] {
        let src = Arc::new(lightcraft_scenes::demo_library()[0].render(w, h));
        let req = RenderRequest { quality, ..RenderRequest::fit(w, h) };
        let stages = StageCache::default();
        let pixels = lightcraft_gpu::render(&src, &raw, &s, &req, Some(&stages)).expect("gpu render");
        match lightcraft_gpu::render_display(&src, &raw, &s, &req, Some(&stages)).expect("display render") {
            Output::Display { frame, histogram } => {
                assert_eq!((frame.width, frame.height), (pixels.image.width, pixels.image.height));
                let shown = lightcraft_gpu::read_texture(&device, &queue, &frame.texture, frame.width, frame.height).expect("readback");
                assert!(shown.data == pixels.image.data, "{w}×{h}: the texture holds the readback's pixels");
                assert_eq!(histogram, pixels.histogram);
            }
            Output::Image(_) => panic!("{w}×{h}: expected a texture"),
        }
    }
    // overlays are drawn on the CPU, and a window that spots grew is cut there: pixels
    let src = Arc::new(lightcraft_scenes::demo_library()[0].render(3000, 2000));
    let req = RenderRequest { overlay: Overlay::Spots(50), ..RenderRequest::fit(800, 800) };
    assert!(matches!(lightcraft_gpu::render_display(&src, &raw, &s, &req, None), Some(Output::Image(_))), "overlay");
    s.spots = vec![Spot { points: vec![Point::new(0.5, 0.5)], size: 0.02, source_offset: Some(Point::new(0.15, 0.0)), ..Default::default() }];
    let req = RenderRequest { window: Some(PixelWindow { x: 1200, y: 800, w: 500, h: 400 }), ..RenderRequest::fit(3000, 2000) };
    match lightcraft_gpu::render_display(&src, &raw, &s, &req, None) {
        Some(Output::Image(r)) => assert_eq!((r.image.width, r.image.height), (500, 400)),
        _ => panic!("a grown window comes back as pixels"),
    }
}
