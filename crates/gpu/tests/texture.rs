//! Issue #1228: renders on a device the window lends (`share_window_device`) reach the window as
//! textures, with no copy back to the CPU. The texture holds exactly the pixels the read-back
//! render returns, for a whole frame, a zoom window and a window that spots grew (and then cut
//! back), and a render that can't be trusted still falls back.
//! Its own test binary: the window's device has to be lent before anything creates the render
//! device. Skips (passes with a note) when no GPU adapter exists.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Spot};
use lightcraft_geom::Point;
use lightcraft_gpu::{Fallback, Fault, GpuTexture};
use lightcraft_pipeline::{Overlay, PixelWindow, RenderRequest, SourceInfo};

/// A device as the window creates it: the default backends, the adapter's limits.
fn window_device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    let backends = lightcraft_gpu::backend::compute_backends()?;
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    )
    .ok()?;
    let limits = lightcraft_gpu::window_device_limits(&adapter);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("test window device"),
        required_limits: limits,
        ..Default::default()
    }))
    .ok()?;
    Some((device, queue, adapter.get_info()))
}

/// The texture's pixels, read back for the comparison (independently of `read_texture`).
fn pixels(device: &wgpu::Device, queue: &wgpu::Queue, t: &GpuTexture) -> Vec<[u8; 4]> {
    let stride = (t.width * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (stride * t.height) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        t.texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(stride as u32), rows_per_image: Some(t.height as u32) },
        },
        wgpu::Extent3d { width: t.width as u32, height: t.height as u32, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let data = slice.get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity(t.width * t.height);
    for row in data.chunks(stride) {
        out.extend_from_slice(row[..t.width * 4].as_chunks::<4>().0);
    }
    out
}

#[test]
fn renders_reach_the_window_as_textures_identical_to_the_read_back_pixels() {
    let Some((device, queue, info)) = window_device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    if info.device_type == wgpu::DeviceType::Cpu {
        eprintln!("skipped: software adapter ({})", info.name);
        return;
    }
    // before the device is lent, nothing renders into textures
    let fit = RenderRequest::fit(900, 600);
    assert!(!lightcraft_gpu::shares_window_device());
    assert!(!lightcraft_gpu::texture_handles(&fit));
    lightcraft_gpu::share_window_device(device.clone(), queue.clone(), info);
    assert!(lightcraft_gpu::available(), "{:?}", lightcraft_gpu::unavailable_reason());
    assert!(lightcraft_gpu::shares_window_device(), "renders run on the lent device");
    assert!(lightcraft_gpu::texture_handles(&fit));

    let src = Arc::new(lightcraft_scenes::demo_library()[0].render(960, 640));
    let srci = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.4;
    s.effects.clarity = 25.0;
    let mut spotted = s.clone();
    spotted.spots.push(Spot {
        points: vec![Point::new(0.45, 0.45)],
        size: 0.03,
        feather: 30.0,
        opacity: 100.0,
        source_offset: Some(Point::new(0.15, 0.0)),
        ..Default::default()
    });
    // a window narrower than the copy alignment's 64 px and of an odd height: padded rows
    let window = |x, y, w, h| RenderRequest { window: Some(PixelWindow { x, y, w, h }), ..RenderRequest::fit(3000, 2000) };
    let cases: [(&str, &DevelopSettings, RenderRequest); 4] = [
        ("whole frame", &s, fit),
        ("zoom window", &s, window(1200, 800, 640, 480)),
        ("narrow odd window", &s, window(1300, 900, 37, 51)),
        ("window grown by a spot", &spotted, window(1200, 800, 640, 480)),
    ];
    for (name, s, req) in &cases {
        let read = lightcraft_gpu::try_render(&src, &srci, s, req, None).unwrap_or_else(|e| panic!("{name}: read-back render: {e:?}"));
        let tex = lightcraft_gpu::try_render_texture(&src, &srci, s, req, None, None).unwrap_or_else(|e| panic!("{name}: texture render: {e:?}"));
        assert_eq!((tex.width, tex.height), (read.image.width, read.image.height), "{name}: size");
        let got = pixels(&device, &queue, &tex);
        let differ: Vec<usize> = got.iter().zip(&read.image.data).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
        assert!(
            differ.is_empty(),
            "{name}: {} of {} pixels differ from the read-back ones, first at {:?}: {:?} vs {:?}",
            differ.len(),
            got.len(),
            differ.first(),
            differ.first().map(|&i| got[i]),
            differ.first().map(|&i| read.image.data[i])
        );
        // what a headless screenshot reads back
        assert_eq!(lightcraft_gpu::read_texture(&tex).map(|i| i.data), Some(got), "{name}: read_texture");
    }

    // a display profile's per-channel curves are applied on the way into the texture, as
    // `DisplayProfile::correct` applies them on the CPU (a different table per channel)
    let mut curves = [[0u8; 256]; 3];
    for (c, t) in curves.iter_mut().enumerate() {
        for (i, v) in t.iter_mut().enumerate() {
            *v = match c {
                0 => 255 - i as u8,
                1 => ((i as f32 / 255.0).powf(1.8) * 255.0).round() as u8,
                _ => (i as u8).saturating_add(17),
            };
        }
    }
    let req = window(1200, 800, 640, 480);
    let read = lightcraft_gpu::try_render(&src, &srci, &s, &req, None).expect("read-back render");
    let want: Vec<[u8; 4]> =
        read.image.data.iter().map(|p| [curves[0][p[0] as usize], curves[1][p[1] as usize], curves[2][p[2] as usize], p[3]]).collect();
    let tex = lightcraft_gpu::try_render_texture(&src, &srci, &s, &req, None, Some(&curves)).expect("texture render with curves");
    assert!(pixels(&device, &queue, &tex) == want, "the display curves differ from the CPU's");

    // overlays are drawn on read-back pixels: not tried as a texture
    let overlay = RenderRequest { overlay: Overlay::Spots(50), ..RenderRequest::fit(900, 600) };
    assert!(!lightcraft_gpu::texture_handles(&overlay));
    assert!(matches!(lightcraft_gpu::try_render_texture(&src, &srci, &s, &overlay, None, None), Err(Fallback::NotTried)));

    // work that never ran is caught without the image coming back (simulated: this thread only)
    lightcraft_gpu::inject_fault(Fault::DropWork);
    match lightcraft_gpu::try_render_texture(&src, &srci, &s, &fit, None, None) {
        Err(Fallback::Failed(why)) => assert!(why.contains("incomplete"), "{why}"),
        other => panic!("dropped work must fall back, got {:?}", other.err()),
    }
    lightcraft_gpu::reset_failures();
    // an output over the device limits falls back without a device error
    lightcraft_gpu::inject_fault(Fault::Limit(1 << 20));
    assert!(matches!(lightcraft_gpu::try_render_texture(&src, &srci, &s, &fit, None, None), Err(Fallback::Failed(_))));
    assert!(lightcraft_gpu::try_render_texture(&src, &srci, &s, &fit, None, None).is_ok(), "a limit is no device failure");
}
