//! On the main thread (as in the CLI), with no event loop running: the camera calls spin the run
//! loop themselves, so ImageCaptureCore's answers arrive. Runs without libtest's harness so that
//! `main` really is the main thread.

fn main() {
    #[cfg(target_os = "macos")]
    {
        use std::time::{Duration, Instant};
        let t = Instant::now();
        // the browser starts and reports what is connected (on CI and most desks: nothing)
        let cams = lightcraft_cameras::cameras();
        assert!(cams.is_ok(), "{cams:?}");
        assert!(t.elapsed() < Duration::from_secs(5), "listing took {:?}", t.elapsed());
        // an unknown camera is a plain error, not a hang
        match lightcraft_cameras::Session::open("no-such-camera") {
            Err(lightcraft_cameras::Error::Device(m)) => assert!(m.contains("isn't connected"), "{m}"),
            Err(e) => panic!("unexpected error {e}"),
            Ok(_) => panic!("opened a camera that doesn't exist"),
        }
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        println!("main thread: {} camera(s), unknown id refused, in {:?}", cams.map(|c| c.len()).unwrap_or(0), t.elapsed());
    }
}
