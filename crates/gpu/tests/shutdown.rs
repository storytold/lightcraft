//! Issue #620: a render still inside the GPU driver while the process exits crashes there (a
//! SIGSEGV no panic hook catches). Once `begin_shutdown` is called nothing starts on the GPU, and
//! `wait_idle` returns when the renders in flight on other threads have ended.
//! One test: the shutdown holds for the whole process. Without a GPU adapter only the CPU side of
//! the contract is checked.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::{RenderRequest, SourceInfo};

#[test]
fn nothing_renders_on_the_gpu_once_the_process_is_ending() {
    let (w, h) = (600, 400);
    let src = Arc::new(lightcraft_scenes::demo_library()[0].render(w, h));
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.3;
    let req = RenderRequest::fit(w, h);
    let has_gpu = lightcraft_gpu::available();
    if !has_gpu {
        eprintln!("no GPU adapter ({:?}): only the CPU side is checked", lightcraft_gpu::unavailable_reason());
    }
    assert!(!lightcraft_gpu::shutting_down());
    assert!(lightcraft_gpu::wait_idle(Duration::ZERO), "nothing is in flight yet");

    // a worker that renders until the GPU refuses, as the preview workers do while the app quits
    let (rendered, refused) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
    let worker = {
        let (src, info, s) = (src.clone(), info.clone(), s.clone());
        let (rendered, refused) = (rendered.clone(), refused.clone());
        std::thread::spawn(move || {
            while lightcraft_gpu::render(&src, &info, &s, &req, None).is_some() {
                rendered.fetch_add(1, Ordering::SeqCst);
            }
            refused.store(true, Ordering::SeqCst);
        })
    };
    if has_gpu {
        while rendered.load(Ordering::SeqCst) == 0 && !refused.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        assert!(rendered.load(Ordering::SeqCst) > 0, "the GPU renders before the shutdown ({:?})", lightcraft_gpu::last_fallback());
    }

    lightcraft_gpu::begin_shutdown();
    assert!(lightcraft_gpu::shutting_down());
    assert!(lightcraft_gpu::wait_idle(Duration::from_secs(60)), "the render in flight ends");
    worker.join().unwrap();
    assert!(refused.load(Ordering::SeqCst));
    assert!(lightcraft_gpu::render(&src, &info, &s, &req, None).is_none(), "no GPU render once the process is ending");
    assert!(lightcraft_gpu::wait_idle(Duration::ZERO));
    if has_gpu {
        assert_eq!(lightcraft_gpu::unavailable_reason().as_deref(), Some("LightCraft is closing"));
    }
}
