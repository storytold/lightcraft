//! Time to first frame through the whole app, on a large library (plan item 4.1, issue #979).
//!
//! `cargo run --release -p lcc-ui-egui --example startup_bench -- [DIR] [PHOTOS]`
//!
//! Makes a synthetic library of `PHOTOS` (default 100 000) photos in `DIR` (default
//! `target/bench/startup-<PHOTOS>`) when it has none — raws of a few camera bodies, a third of them
//! edited, keywords and ratings, like a library brought over from another program — then starts the
//! app on it the way the desktop app does: an unopened session, the window's first frame, the
//! catalog read and its first view sorted on a worker
//! ([`lcc_ui_egui::panels::library_problem::start_open`]), the session
//! opened on the interface's thread, and the first frames of the grid after it. A headless host draws
//! the frames (no window, no GPU surface: the window's own creation is not counted).
//!
//! Prints, in the `name: N ms wall` form `cargo xtask bench` reads:
//! - `startup first frame`: from creating the app to the end of its first frame (what the window
//!   shows at once);
//! - `startup library ready`: from creating the app until the library is open and its grid has been
//!   drawn — the time to a usable first frame, budgeted at ≤ 3 s for 100 000 photos;
//! - `startup prep tick`: the slowest of the background preparation's first 30 ticks after the
//!   library opened, in a session of its own (it runs every frame);
//! - `startup slowest frame`: the longest single frame on the way and in the second after (a frame
//!   over 100 ms is a stall the user sees).
//!
//! The first open in a fresh process is measured; the OS file cache is not flushed, so a run straight
//! after making the library reads it from memory.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use lcc_catalog::{MediaKind, Op, Photo, Source};
use lcc_engine::Session;
use lcc_ui_egui::headless::Headless;
use lcc_ui_egui::{LccApp, Services};

const BODIES: [(&str, &str); 4] =
    [("Canon EOS R5", "CR3"), ("SONY ILCE-7M4", "ARW"), ("NIKON CORPORATION NIKON Z 6_2", "NEF"), ("FUJIFILM X-T4", "RAF")];

fn make_library(dir: &str, n: u32) {
    let t = Instant::now();
    let mut s = Session::new().with_fs();
    s.open_library(dir, false).expect("create library");
    for i in 0..n {
        let id = s.catalog.alloc_photo_id();
        let (camera, ext) = BODIES[i as usize % BODIES.len()];
        let path = format!("/photos/{}/{}/IMG_{i:06}.{ext}", 2000 + i % 25, i % 365);
        let mut p = Photo::new(id, Source::File { path }, &format!("IMG_{i:06}.{ext}"), ext, 6000, 4000, "2026-01-01T00:00:00");
        p.kind = MediaKind::Raw;
        p.meta.camera = camera.into();
        p.as_shot_wb = Some((5000.0 + f64::from(i % 1500), f64::from(i % 7)));
        p.develop = std::sync::Arc::new(p.camera_defaults());
        p.file_size = 30_000_000 + u64::from(i);
        p.captured = Some(format!("{}-{:02}-{:02}T10:00:00", 2000 + i % 25, 1 + i % 12, 1 + i % 28));
        p.meta.keywords = vec![format!("kw{}", i % 500), format!("place{}", i % 50)];
        p.rating = (i % 6) as u8;
        if i % 3 == 0 {
            p.edited = Some("2026-01-02T00:00:00".into());
            let mut d = (*p.develop).clone();
            d.light.exposure = 0.3;
            p.develop = std::sync::Arc::new(d);
        }
        let _ = s.catalog.apply(Op::AddPhoto { photo: Box::new(p) });
    }
    s.close_library().expect("close library");
    eprintln!("made a library of {n} photos in {dir} in {:.1} s", t.elapsed().as_secs_f64());
}

/// With `<PREFIX>_PROFILE=1`, what the app logs at info level (the library open's stages) goes to
/// stderr, so a slow frame can be traced to its stage.
struct StderrLog;
impl log::Log for StderrLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("[{}] {}", r.target(), r.args());
        }
    }
    fn flush(&self) {}
}
static STDERR_LOG: StderrLog = StderrLog;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() {
    if lcc_pipeline::profiling() && log::set_logger(&STDERR_LOG).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n: u32 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(100_000);
    let dir = args.first().cloned().unwrap_or_else(|| format!("target/bench/startup-{n}"));
    if !lcc_engine::library::is_library(std::path::Path::new(&dir)) {
        std::fs::create_dir_all(&dir).expect("library folder");
        make_library(&dir, n);
    }
    let t0 = Instant::now();
    let mut app = LccApp::new(Session::new().with_fs().with_system_clock(), Services::default());
    app.ui.settings.library_path = dir.clone();
    lcc_ui_egui::panels::library_problem::start_open(&mut app, dir.clone().into(), Vec::new(), false, false);
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    let mut slowest = 0.0f64;
    let profile = lcc_pipeline::profiling();
    let mut frame = |h: &mut Headless| {
        let t = Instant::now();
        h.step();
        let took = ms(t.elapsed());
        if profile && took > 100.0 {
            eprintln!("startup: a {took:.1} ms frame at {:.1} ms", ms(t0.elapsed()));
        }
        slowest = slowest.max(took);
    };
    frame(&mut h);
    let first = t0.elapsed();
    let open = |h: &Headless| h.app.library_problem.is_none() && h.app.session.library.as_ref().is_some_and(|l| l.on_disk);
    while !open(&h) && t0.elapsed() < Duration::from_secs(600) {
        std::thread::sleep(Duration::from_millis(2));
        frame(&mut h);
        if let Some(e) = h.app.library_problem.as_ref().map(|p| p.error.clone()).filter(|e| !e.is_empty()) {
            eprintln!("the library did not open: {e}");
            std::process::exit(1);
        }
    }
    if !open(&h) {
        eprintln!("the library did not open within 10 minutes");
        std::process::exit(1);
    }
    // the frame that opened it swapped the session in; the next one draws the grid
    frame(&mut h);
    let ready = t0.elapsed();
    let photos = h.app.session.catalog.len();
    // a second of frames after: what the first frames of the grid set going (background
    // preparation, the edited photos' previews looked up) must not stall the interface either
    let after = Instant::now();
    while after.elapsed() < Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(16));
        frame(&mut h);
    }
    drop(h);
    // the background preparation's tick on its own (run every frame): the first ones after opening,
    // in a session of its own; the slowest of 30 is the stall it can add to a frame (#979)
    let mut s = Session::new().with_fs().with_system_clock();
    s.open_library(&dir, false).expect("open library");
    let mut tick = 0.0f64;
    for _ in 0..30 {
        let t = Instant::now();
        s.prep_tick(lcc_engine::prep::Mode::Library, None, false);
        tick = tick.max(ms(t.elapsed()));
    }
    println!("library: {photos} photos");
    println!("startup prep tick: {tick:.1} ms wall");
    println!("startup first frame: {:.1} ms wall", ms(first));
    println!("startup library ready: {:.1} ms wall", ms(ready));
    println!("startup slowest frame: {slowest:.1} ms wall");
}
