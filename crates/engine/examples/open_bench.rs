//! How long opening a large library takes, stage by stage (issue #979). Makes a library of N
//! photos in `<dir>` (when it has none), then opens it twice and prints the stages.
//!
//! `<PREFIX>_PROFILE=1 cargo run --release -p lcc-engine --example open_bench -- <dir> [photos]`
use std::time::Instant;

use lcc_catalog::{Op, Photo, Source};
use lcc_engine::Session;

fn main() {
    let _ = env_logger_lite();
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: open_bench <dir> [photos]");
        return;
    };
    let n: u32 = std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(300_000);
    if !lcc_engine::library::is_library(std::path::Path::new(&dir)) {
        let mut s = Session::new().with_fs();
        if let Err(e) = s.open_library(&dir, false) {
            eprintln!("{e}");
            return;
        }
        for i in 0..n {
            let id = s.catalog.alloc_photo_id();
            let path = format!("/photos/{}/{}/IMG_{i:06}.CR2", 2000 + i % 25, i % 365);
            let mut p = Photo::new(id, Source::File { path }, &format!("IMG_{i:06}.CR2"), "CR2", 6000, 4000, "2026-01-01T00:00:00");
            p.captured = Some(format!("{}-{:02}-{:02}T10:00:00", 2000 + i % 25, 1 + i % 12, 1 + i % 28));
            p.meta.keywords = vec![format!("kw{}", i % 500), format!("place{}", i % 50)];
            p.rating = (i % 6) as u8;
            if i % 3 == 0 {
                p.edited = Some("2026-01-02T00:00:00".into());
            }
            let _ = s.catalog.apply(Op::AddPhoto { photo: Box::new(p) });
        }
        let t = Instant::now();
        let _ = s.close_library();
        println!("made {n} photos, closed in {:.1} s", t.elapsed().as_secs_f64());
    }
    for run in 0..2 {
        let mut s = Session::new().with_fs();
        let t = Instant::now();
        match s.open_library(&dir, false) {
            Ok(_) => println!("run {run}: opened {} photos in {:.1} ms", s.catalog.len(), t.elapsed().as_secs_f64() * 1000.0),
            Err(e) => eprintln!("{e}"),
        }
        let t = Instant::now();
        let _ = s.visible();
        println!("run {run}: first visible list {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    }
}

/// Print `log` records to stderr (the stage timings are logged).
fn env_logger_lite() -> Result<(), log::SetLoggerError> {
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            eprintln!("{} {}", r.level(), r.args());
        }
        fn flush(&self) {}
    }
    log::set_logger(&L).map(|()| log::set_max_level(log::LevelFilter::Info))
}
