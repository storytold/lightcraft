//! A test is a process of its own under nextest, and that process ends as soon as the test
//! returns. The app's first frame starts the GPU device warm-up on a thread of its own; a process
//! that ends while that thread is still loading the driver crashes there (issue #620; in CI,
//! tests killed by SIGSEGV after they had passed). So dropping the app waits for a warm-up that
//! is still in flight. This exercises that wait with a substitute warm-up held on the same gate,
//! so it needs no GPU, no device and no race.

use std::sync::mpsc;
use std::time::Duration;

use crate::{LightcraftApp, Services};

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn dropping_the_app_waits_for_a_warm_up_that_is_in_flight() {
    // In flight from here until `hold` is dropped (test support in the gpu crate, on the gate the
    // real warm-up enters)
    let hold = lightcraft_engine::gpu::hold_warm_up_for_test().expect("the warm-up gate is open in a test");
    assert!(!lightcraft_engine::gpu::wait_warm_up(Duration::ZERO), "the substitute warm-up is in flight");
    let (tx, rx) = mpsc::channel::<&'static str>();
    let dropping = std::thread::spawn(move || {
        // Built here, not moved in: `LightcraftApp` is not `Send` (its host-service callbacks are
        // not), and this test does not need it to be. An app that never ran a frame has nothing of
        // its own pending, so the only thing that can hold up its drop is the warm-up wait.
        let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
        tx.send("dropping").unwrap();
        drop(app);
        tx.send("dropped").unwrap();
    });
    assert_eq!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), "dropping");
    // The drop has not returned while the warm-up is held. The one second is only the
    // observation window for that — the thread is already past "dropping", sent after the
    // construction and just before the drop, so an unwaiting drop would have torn the app down
    // and returned well inside it — and the proof is the step after: releasing the hold is what
    // lets the drop return.
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "dropping the app returned while a warm-up was in flight"
    );
    drop(hold);
    assert_eq!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), "dropped");
    dropping.join().unwrap();
}
