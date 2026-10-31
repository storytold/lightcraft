//! A lost device doesn't turn the GPU off for the session (TEST-NOTES 2026-10-15, note 43: one
//! failure during a 571-photo export left every later photo on the CPU): the renders while it is
//! lost go to the CPU, a new device replaces it after a short pause, renders use the GPU again, and
//! each step is told in plain words. Its own process: it replaces the device the others render on.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lightcraft_develop::DevelopSettings;
use lightcraft_gpu::{EventKind, Fallback};
use lightcraft_pipeline::{RenderRequest, SourceInfo, StageCache, render};
use lightcraft_raster::Rgba8;

fn mean_abs_diff(a: &Rgba8, b: &Rgba8) -> f64 {
    let sum: u64 = a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|c| p[c].abs_diff(q[c]) as u64).sum::<u64>()).sum();
    sum as f64 / (a.data.len() * 3) as f64
}

#[test]
fn a_lost_device_is_replaced_and_used_again() {
    let made = Instant::now();
    if !lightcraft_gpu::available() {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter ({})", lightcraft_gpu::unavailable_reason().unwrap_or_default());
        return;
    }
    eprintln!("device + kernels: {:?}", made.elapsed());
    let (w, h) = (640, 427);
    let src = Arc::new(lightcraft_scenes::demo_library()[2].render(w, h));
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.4;
    s.effects.clarity = 25.0;
    s.detail.nr_luminance = 20.0;
    let req = RenderRequest::fit(w, h);
    let cpu = render(&src, &info, &s, &req).image;
    // a view's stages, cached on the first device
    let stages = StageCache::default();
    let first = lightcraft_gpu::try_render(&src, &info, &s, &req, Some(&stages)).expect("renders on the GPU");
    assert!(mean_abs_diff(&first.image, &cpu) < 0.5);
    assert!(lightcraft_gpu::stage_bytes(&stages) > 0, "the view keeps device stages");
    let generation = lightcraft_gpu::device_generation().expect("a device");
    let seen = lightcraft_gpu::events_after(0).last().map_or(0, |e| e.seq);

    lightcraft_gpu::simulate_device_loss("device lost (Unknown): simulated driver reset");
    // while it is lost: the CPU renders, and the status says why in plain words
    assert!(!lightcraft_gpu::enabled());
    assert_eq!(lightcraft_gpu::try_render(&src, &info, &s, &req, Some(&stages)).err(), Some(Fallback::NotTried));
    let why = lightcraft_gpu::unavailable_reason().unwrap_or_default();
    assert!(why.contains("restarting the graphics card") && why.contains("simulated driver reset"), "{why}");
    let lost: Vec<_> = lightcraft_gpu::events_after(seen);
    assert_eq!(lost.len(), 1, "{lost:?}");
    assert_eq!(lost[0].kind, EventKind::Lost);
    assert!(lost[0].text.contains("made on the processor instead"), "{}", lost[0].text);
    // a second report of the same loss says nothing more
    lightcraft_gpu::simulate_device_loss("again");
    assert_eq!(lightcraft_gpu::events_after(seen).len(), 1);

    // after the pause a new device renders, the old one's cached stages are not used on it
    let t = Instant::now();
    let again = loop {
        match lightcraft_gpu::try_render(&src, &info, &s, &req, Some(&stages)) {
            Ok(r) => break r,
            // (building the kernels again can take as long as at the start)
            Err(_) if t.elapsed() < Duration::from_secs(180) => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => panic!("the GPU did not come back: {e:?} ({:?})", lightcraft_gpu::unavailable_reason()),
        }
    };
    assert!(t.elapsed() >= Duration::from_secs(2), "not before the pause: {:?}", t.elapsed());
    eprintln!("back on the GPU after {:?}", t.elapsed());
    assert!(lightcraft_gpu::device_generation().expect("a device") > generation, "a new device");
    assert_eq!(lightcraft_gpu::recoveries(), 1);
    assert!(lightcraft_gpu::enabled() && lightcraft_gpu::unavailable_reason().is_none());
    assert!(mean_abs_diff(&again.image, &cpu) < 0.5, "the new device renders the same picture");
    assert!(mean_abs_diff(&again.image, &first.image) < 0.01);
    let events = lightcraft_gpu::events_after(seen);
    assert_eq!(events.last().map(|e| e.kind), Some(EventKind::Recovered), "{events:?}");
    // and keeps rendering, from its own stages
    let third = lightcraft_gpu::try_render(&src, &info, &s, &req, Some(&stages)).expect("still on the GPU");
    assert!(mean_abs_diff(&third.image, &again.image) < 0.01);
}
