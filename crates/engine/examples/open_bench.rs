//! How long opening a large library takes, stage by stage (issue #979). Makes a library of N
//! photos in `<dir>` (when it has none), then opens it twice and prints the stages.
//!
//! `<PREFIX>_PROFILE=1 cargo run --release -p lcc-engine --example open_bench -- <dir> [photos] [thumb KB]`
//!
//! The photos are raws of a few camera bodies with an as-shot white balance, a third of them
//! edited, like a library brought over from another program. With `thumb KB` above 0 the
//! library also gets a thumbnail cache of one file of that size per photo (a reporter's
//! 328 000-photo library had 18 GB of them, some 55 KB each), so a stage that reads the cache
//! shows.
use std::time::Instant;

use lcc_catalog::{MediaKind, Op, Photo, Source};
use lcc_engine::Session;

const BODIES: [(&str, &str); 4] = [("Canon EOS R5", "CR3"), ("SONY ILCE-7M4", "ARW"), ("NIKON CORPORATION NIKON Z 6_2", "NEF"), ("FUJIFILM X-T4", "RAF")];

fn main() {
    let _ = env_logger_lite();
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: open_bench <dir> [photos] [thumb KB]");
        return;
    };
    let n: u32 = std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(300_000);
    let thumb_kb: usize = std::env::args().nth(3).and_then(|a| a.parse().ok()).unwrap_or(0);
    if !lcc_engine::library::is_library(std::path::Path::new(&dir)) {
        let mut s = Session::new().with_fs();
        if let Err(e) = s.open_library(&dir, false) {
            eprintln!("{e}");
            return;
        }
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
        let t = Instant::now();
        let _ = s.close_library();
        println!("made {n} photos, closed in {:.1} s", t.elapsed().as_secs_f64());
        if thumb_kb > 0 {
            let t = Instant::now();
            match write_thumbs(&std::path::Path::new(&dir).join("thumbs"), n as usize, thumb_kb << 10) {
                Ok(()) => println!("wrote {n} thumbnails of {thumb_kb} KB in {:.1} s", t.elapsed().as_secs_f64()),
                Err(e) => eprintln!("thumbnails: {e}"),
            }
        }
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
        // the edited photos' previews are found in the background after a large library opens
        let t = Instant::now();
        while s.media.rendered.disk().is_some_and(|d| d.is_keep_last_pending()) && t.elapsed().as_secs() < 600 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        println!("run {run}: edited photos found in the background {:.1} ms later", t.elapsed().as_secs_f64() * 1000.0);
    }
}

/// `count` cache files of `bytes` each under `dir`, in the cache's own layout
/// (`<2 hex>/<32 hex>.lcthumb`), written on all cores.
fn write_thumbs(dir: &std::path::Path, count: usize, bytes: usize) -> std::io::Result<()> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let body = vec![0x5au8; bytes];
    std::thread::scope(|sc| {
        let jobs: Vec<_> = (0..threads)
            .map(|t| {
                let body = &body;
                sc.spawn(move || -> std::io::Result<()> {
                    for i in (t..count).step_by(threads) {
                        // (a key spread over all 256 shards like the cache's hashes)
                        let key = (i as u128).wrapping_mul(0x9e37_79b9_7f4a_7c15_f39c_c060_5ced_c835);
                        let hex = format!("{key:032x}");
                        let shard = dir.join(hex.get(..2).unwrap_or("00"));
                        std::fs::create_dir_all(&shard)?;
                        std::fs::write(shard.join(format!("{hex}.lcthumb")), body)?;
                    }
                    Ok(())
                })
            })
            .collect();
        jobs.into_iter().try_for_each(|j| j.join().unwrap_or_else(|_| Err(std::io::Error::other("writer panicked"))))
    })
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
