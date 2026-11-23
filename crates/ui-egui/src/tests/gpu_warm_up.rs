//! A test is a process of its own under nextest, and that process ends as soon as the test
//! returns. The app's first frame starts the GPU device warm-up on a thread of its own; a process
//! that ends while that thread is still loading the driver crashes there (issue #620; in CI,
//! tests killed by SIGSEGV after they had passed). So dropping the app must leave no warm-up
//! running.

use crate::{LightcraftApp, Services};

#[test]
fn dropping_the_app_leaves_no_gpu_device_creation_running() {
    let ctx = egui::Context::default();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    assert!(app.ui.settings.gpu, "GPU rendering is on by default, so the first frame starts the warm-up");
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
    out.textures_delta.clear();
    drop(app);
    // checked at once: nothing waits between the end of a test and the end of its process
    assert!(lightcraft_engine::gpu::ready() || !lightcraft_engine::gpu::enabled(), "the GPU device is still being created after the app is gone");
}
