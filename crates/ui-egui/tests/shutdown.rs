//! Issue #620: quitting while previews render must not leave a render worker running (on Linux
//! with the NVIDIA driver, one inside the GPU when the process exits is a SIGSEGV).
//! `LightcraftApp::shutdown` stops the render workers and closes the GPU for the whole process,
//! hence one test in a binary of its own.

use std::time::{Duration, Instant};

use lightcraft_ui_egui::headless::Headless;
use lightcraft_ui_egui::{LightcraftApp, Services};

#[test]
fn shutdown_leaves_no_render_worker_running() {
    // what the host was told about jobs holding the machine awake
    let told = std::rc::Rc::new(std::cell::RefCell::new(Vec::<Option<String>>::new()));
    let sink = told.clone();
    let keep_awake: lightcraft_ui_egui::KeepAwakeFn = Box::new(move |reason| sink.borrow_mut().push(reason.map(str::to_owned)));
    let services = Services { png: None, keep_awake: Some(keep_awake), ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1200.0, 760.0], 1.0);
    // frames until the grid's thumbnails are being rendered on the workers
    let t0 = Instant::now();
    while h.app.renderer.live_workers() == 0 || h.app.renderer.in_flight() == 0 {
        assert!(t0.elapsed() < Duration::from_secs(120), "no render started");
        h.step();
    }
    assert!(!lightcraft_engine::gpu::shutting_down());
    // a job is running when the app quits: the hold on the machine ends with it
    let _job = h.app.session.activity.start("export", "Exporting", lightcraft_engine::activity::Cancel::Yes);
    h.step();
    assert_eq!(told.borrow().last(), Some(&Some("Exporting".to_string())));

    assert!(h.app.shutdown(Duration::from_secs(120)), "the jobs that were running ended in time");
    assert_eq!(told.borrow().last(), Some(&None), "quitting lets go of the hold");
    assert_eq!(h.app.renderer.live_workers(), 0);
    assert_eq!(h.app.renderer.queued(), 0, "jobs that had not started are dropped");
    assert!(lightcraft_engine::gpu::shutting_down());
    assert!(lightcraft_engine::gpu::wait_idle(Duration::ZERO), "nothing is inside the GPU");

    // frames after it (the window's last ones) ask for renders that are not started any more
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(told.borrow().last(), Some(&None), "the job still runs, but the window does not take the hold again");
    assert_eq!(h.app.renderer.live_workers(), 0);
    assert_eq!(h.app.renderer.queued(), 0);

    // a job that was caught before its render gives up: quitting doesn't wait for a CPU render
    let id = h.app.session.active().or_else(|| h.app.session.catalog.photos().next().map(|p| p.id)).expect("a demo photo");
    let job = h.app.session.render_job(id, 300, 200, false, true).expect("a render job");
    assert_eq!(job.run().rendered.err().as_deref(), Some("LightCraft is closing"));
}
