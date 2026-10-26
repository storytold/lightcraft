//! Issue #1228: when renders share the window's device and that device is lost (a driver reset,
//! a GPU hang; simulated with `Device::destroy`), nothing panics: renders go back to the CPU,
//! reading a texture back gives nothing, textures already handed over drop cleanly, and
//! `window_device_lost` tells the app that the window needs a restart.
//! Its own test binary: it lends a device before anything creates one. Skips without a GPU.

use std::sync::Arc;

use lightcraft_develop::DevelopSettings;
use lightcraft_gpu::Fallback;
use lightcraft_pipeline::{RenderRequest, SourceInfo};

#[test]
fn a_lost_window_device_falls_back_without_a_panic() {
    let Some(backends) = lightcraft_gpu::backend::compute_backends() else { return };
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    let instance = wgpu::Instance::new(desc);
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
        eprintln!("skipped: software adapter");
        return;
    }
    let desc = wgpu::DeviceDescriptor { required_limits: lightcraft_gpu::window_device_limits(&adapter), ..Default::default() };
    let (device, queue) = pollster::block_on(adapter.request_device(&desc)).expect("device");
    lightcraft_gpu::share_window_device(device.clone(), queue.clone(), adapter.get_info());
    // (creates the render device on the lent one, as the app's start-up check does)
    assert!(lightcraft_gpu::available() && lightcraft_gpu::shares_window_device());

    let src = Arc::new(lightcraft_scenes::demo_library()[0].render(640, 420));
    let info = SourceInfo { raw: true, ..Default::default() };
    let s = DevelopSettings::default();
    let req = RenderRequest::fit(640, 420);
    let shown = lightcraft_gpu::try_render_texture(&src, &info, &s, &req, None, None).expect("a texture before the loss");
    assert!(!lightcraft_gpu::window_device_lost());

    device.destroy();
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    // (the lost-device callback runs from a poll)
    for _ in 0..100 {
        if lightcraft_gpu::window_device_lost() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = device.poll(wgpu::PollType::Poll);
    }
    assert!(lightcraft_gpu::window_device_lost(), "the app is told the window's device is gone");
    assert!(lightcraft_gpu::unavailable_reason().is_some_and(|r| r.contains("lost")), "{:?}", lightcraft_gpu::unavailable_reason());
    assert!(!lightcraft_gpu::texture_handles(&req) || !lightcraft_gpu::enabled());

    // renders are not tried on the GPU again: the CPU renders them, as pixels
    assert!(matches!(lightcraft_gpu::try_render_texture(&src, &info, &s, &req, None, None), Err(Fallback::NotTried)));
    assert!(matches!(lightcraft_gpu::try_render(&src, &info, &s, &req, None), Err(Fallback::NotTried)));
    let cpu = lightcraft_pipeline::render(&src, &info, &s, &req);
    assert_eq!((cpu.image.width, cpu.image.height), (shown.width, shown.height));
    // a screenshot of what was on screen finds nothing to read, without a panic
    assert!(lightcraft_gpu::read_texture(&shown).is_none());
    lightcraft_gpu::trim_pool(0);
    drop(shown);
}
