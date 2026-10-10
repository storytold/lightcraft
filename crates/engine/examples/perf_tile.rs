//! P6.1 budget probe: a 1:1 loupe tile (`Session::region_job`), budget ≤ 100 ms.
//! `cargo run --release -p dac-engine --example perf_tile -- corpus/raw/arw-sony-a7m3-compressed.arw`
//! Prints: the first window (decodes the original), the same window again, a pan to the next
//! window, and an exposure change on the same window (the source is cached across all three).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use dac_pipeline::PixelWindow;
use serde_json::json;

fn main() {
    let path = std::env::args().nth(1).expect("a raw file");
    let edge: usize = std::env::var("TILE").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    let mut s = dac_engine::Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [path]})).expect("import");
    let id = dac_catalog::PhotoId(r["imported"][0].as_u64().expect("imported"));
    let p = s.catalog.photo(id).expect("photo").clone();
    let (w, h) = (p.width as usize, p.height as usize);
    // as the loupe does: one stage cache per view slot, kept across its renders
    let stages = std::sync::Arc::new(dac_pipeline::StageCache::default());
    let time = |label: &str, s: &mut dac_engine::Session, win: PixelWindow| {
        let t = Instant::now();
        let job = s.region_job(id, w, h, win, true).expect("region job").with_stages(stages.clone());
        let img = job.run().rendered.expect("render").image;
        println!("{label:<34} {:>8.1} ms  ({}×{})", t.elapsed().as_secs_f64() * 1e3, img.width, img.height);
    };
    let win = PixelWindow { x: w / 3, y: h / 3, w: edge, h: edge };
    println!("source {w}×{h}, tile {edge}×{edge}");
    time("1:1 tile, first (decode)", &mut s, win);
    time("1:1 tile, same window", &mut s, win);
    time("1:1 tile, pan right", &mut s, PixelWindow { x: win.x + edge / 2, ..win });
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.7})).ok();
    time("1:1 tile, after exposure change", &mut s, win);
    s.execute("develop.set", &json!({"control": "detail.nrLuminance", "value": 30})).ok();
    time("1:1 tile, after NR change", &mut s, win);
}
