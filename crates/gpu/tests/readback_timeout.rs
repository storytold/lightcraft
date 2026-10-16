//! Issue #1948: a slow submission must not permanently retire a healthy device. Use tiny
//! procedural images and inject the timeout after real submission/map registration, then
//! destroy the actual device as a negative control. Keep this in its own test executable:
//! device loss intentionally affects the entire process and cannot be reset.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lightcraft_develop::DevelopSettings;
use lightcraft_gpu::{Fallback, Fault};
use lightcraft_pipeline::{RenderRequest, SourceInfo, StageCache};
use lightcraft_raster::Rgb32f;

#[test]
fn timed_out_readbacks_recover_but_a_lost_device_does_not() {
    if !lightcraft_gpu::available() {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter ({})", lightcraft_gpu::unavailable_reason().unwrap_or_default());
        return;
    }
    eprintln!("timeout recovery adapter: {}", lightcraft_gpu::adapter_name().unwrap_or_default());
    let (w, h) = (96, 64);
    let info = SourceInfo::default();
    let req = RenderRequest::fit(w, h);
    let stages = StageCache::default();
    let mut settings = DevelopSettings::default();
    settings.light.exposure = 0.3;
    for round in 0..4 {
        // Different images of the same size exercise pooled buffers. Dehaze also forces an
        // early readback: later stages must not submit more work after that timeout.
        let src = Arc::new(Rgb32f::from_fn(w, h, |x, y| [0.03 + (x + round) as f32 / 180.0, 0.02 + y as f32 / 130.0, 0.12 + round as f32 * 0.08]));
        settings.effects.dehaze = if round % 2 == 0 { 0.0 } else { 20.0 };
        lightcraft_gpu::inject_fault(Fault::ReadbackTimeout);
        let started = Instant::now();
        let failed = lightcraft_gpu::try_render(&src, &info, &settings, &req, Some(&stages)).err().expect("injected timeout must fall back");
        let why = failed.reason().expect("the GPU render was attempted");
        assert!(why.contains("timed out"), "{why}");
        assert!(!why.contains("not used again"), "a timeout is not a device loss: {why}");
        assert!(started.elapsed() < Duration::from_secs(10), "fault injection must not wait 60 s");
        assert_eq!(lightcraft_gpu::stage_bytes(&stages), 0, "failed GPU stages must be discarded");
        assert!(lightcraft_gpu::enabled());
        assert_eq!(lightcraft_gpu::unavailable_reason(), None);

        // The shutdown API must drive completion itself even if no other render polls. This
        // also checks staging cancellation does not leave a map failure in the next render.
        if round % 2 == 0 {
            assert!(lightcraft_gpu::wait_idle(Duration::from_secs(10)), "the tiny timed-out submission should drain");
        }
        let gpu = match lightcraft_gpu::try_render(&src, &info, &settings, &req, Some(&stages)) {
            Ok(r) => r,
            Err(Fallback::Failed(why)) => {
                assert!(why.contains("still pending"), "only pending work may delay recovery: {why}");
                assert!(lightcraft_gpu::enabled());
                assert!(lightcraft_gpu::wait_idle(Duration::from_secs(10)));
                lightcraft_gpu::try_render(&src, &info, &settings, &req, Some(&stages)).expect("drained device must recover without reset_failures")
            }
            Err(Fallback::NotTried) => panic!("timeout must not retire the device: {:?}", lightcraft_gpu::unavailable_reason()),
        };
        let cpu = lightcraft_pipeline::render(&src, &info, &settings, &req);
        assert_eq!((gpu.image.width, gpu.image.height), (w, h));
        assert_eq!(gpu.image.data.len(), cpu.image.data.len());
        let sum: u64 = gpu.image.data.iter().zip(&cpu.image.data).map(|(a, b)| (0..3).map(|c| a[c].abs_diff(b[c]) as u64).sum::<u64>()).sum();
        let mean = sum as f64 / (w * h * 3) as f64;
        eprintln!("round {round}: timeout recovered on the same device, CPU mean difference {mean:.3} LSB");
        assert!(mean < 0.5, "round {round}: recovered render differs from CPU by {mean:.3} LSB");
        assert!(gpu.image.data.iter().all(|p| p[3] == 255));
    }

    let src = Arc::new(Rgb32f::from_fn(w, h, |_, _| [0.2, 0.3, 0.4]));
    // Leave a timeout ticket outstanding, then lose the real device before recovery polling.
    // That poll runs outside the normal render's panic guard, including at shutdown.
    lightcraft_gpu::inject_fault(Fault::ReadbackTimeout);
    assert!(
        lightcraft_gpu::try_render(&src, &info, &settings, &req, None)
            .err()
            .and_then(|e| e.reason().map(str::to_owned))
            .is_some_and(|s| s.contains("timed out"))
    );
    lightcraft_gpu::inject_fault(Fault::DeviceLost);
    assert!(lightcraft_gpu::try_render(&src, &info, &settings, &req, None).is_err());
    let _ = lightcraft_gpu::wait_idle(Duration::from_millis(20));
    // Unlike simulated validation/drop-work failures, this is the real wgpu device-lost
    // callback and must remain fatal across reset_failures and across threads.
    lightcraft_gpu::reset_failures();
    assert!(!lightcraft_gpu::available());
    let why = lightcraft_gpu::unavailable_reason().expect("device loss must remain recorded");
    eprintln!("real device-loss control: {why}");
    assert!(why.contains("device lost"), "{why}");
    assert_eq!(lightcraft_gpu::try_render(&src, &info, &settings, &req, None).err(), Some(Fallback::NotTried));
    assert!(!std::thread::spawn(lightcraft_gpu::available).join().unwrap());
}
