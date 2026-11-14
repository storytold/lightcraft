//! What [`Catalog::repair`] costs on a large, sound synthetic catalog: its time, and with
//! `REPAIR_BENCH_HEAP=1` the heap blocks and bytes it allocates (that run's times include dhat's own
//! cost). A catalog this build wrote has nothing to repair, so a repair that allocates anything is
//! copying.
//!
//! ```text
//! cargo run --release -p lightcraft-catalog --example repair_bench -- [photos…]
//! REPAIR_BENCH_HEAP=1 cargo run --release -p lightcraft-catalog --example repair_bench -- [photos…]
//! ```
//!
//! Default size: 50000. Two cases per size: `loaded` (the catalog just read from its snapshot, so
//! it holds the only reference to every photo) and `shared` (something else keeps a reference to
//! every photo, as a catalog clone does: `Arc::make_mut` then copies whatever a repair touches).

use std::sync::Arc;
use std::time::Instant;

use lightcraft_catalog::{Catalog, HistoryStep, MediaKind, Op, Photo, Source};
use lightcraft_develop::DevelopSettings;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn photo(c: &mut Catalog, i: u64) -> Photo {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(
        id,
        Source::File { path: format!("/photos/2026/09/{:05}/IMG_{i:05}.CR3", i / 500) },
        &format!("IMG_{i:05}.CR3"),
        "CR3",
        6000,
        4000,
        "2026-09-30T12:00:00",
    );
    p.kind = MediaKind::Raw;
    p.file_size = 25_000_000 + i;
    p.captured = Some(format!("2026-09-{:02}T10:{:02}:{:02}", 1 + i % 28, i % 60, (i / 60) % 60));
    p.meta.camera = "Camera Model X".into();
    p.meta.lens = "24-70mm f/2.8".into();
    p.develop = Arc::new(p.import_defaults());
    if i.is_multiple_of(10) {
        // an edited photo with a few History steps
        let mut d = DevelopSettings::default();
        d.light.exposure = (i % 7) as f64 * 0.1;
        let s = Arc::new(d);
        p.develop = s.clone();
        p.edited = Some("2026-09-30T12:00:00".into());
        p.history = (0..5).map(|_| HistoryStep { label: "Exposure".into(), settings: s.clone() }).collect();
    }
    p
}

fn catalog(n: u64) -> Result<Catalog, Box<dyn std::error::Error>> {
    let mut c = Catalog::new();
    for i in 0..n {
        let p = photo(&mut c, i);
        c.apply(Op::AddPhoto { photo: Box::new(p) })?;
    }
    Ok(c)
}

/// One `repair` of `c`, with what it took. With `heap`, also the heap blocks and bytes it allocated.
fn measure(label: &str, mut c: Catalog, hold: bool, heap: bool) {
    let held = hold.then(|| c.clone());
    let before = heap.then(dhat::HeapStats::get);
    let t = Instant::now();
    let changed = c.repair();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    let mut line = format!("  {label:7} repair {ms:9.2} ms ({} repaired)", changed.len());
    if let Some(before) = before {
        let now = dhat::HeapStats::get();
        line += &format!(
            ": {} blocks, {:.2} MB allocated, {:+.2} MB live afterwards",
            now.total_blocks - before.total_blocks,
            (now.total_bytes - before.total_bytes) as f64 / 1e6,
            (now.curr_bytes as f64 - before.curr_bytes as f64) / 1e6
        );
    }
    println!("{line}");
    drop(held);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut sizes: Vec<u64> = std::env::args().skip(1).map(|a| a.parse()).collect::<Result<_, _>>()?;
    if sizes.is_empty() {
        sizes = vec![50_000];
    }
    // Build every snapshot first: dhat counts only once its profiler exists, and it slows everything after.
    let snapshots: Vec<(u64, String)> =
        sizes.iter().map(|n| Ok((*n, catalog(*n)?.to_snapshot()))).collect::<Result<_, Box<dyn std::error::Error>>>()?;
    // `REPAIR_BENCH_HEAP=1` counts heap blocks and bytes; the times of that run include dhat's own cost.
    let heap = std::env::var_os("REPAIR_BENCH_HEAP").is_some();
    let _profiler = heap.then(|| dhat::Profiler::builder().testing().build());
    for (n, json) in snapshots {
        println!("== {n} photos (snapshot {:.1} MB)", json.len() as f64 / 1e6);
        for hold in [false, true] {
            // read without repairing, so that `repair` is what is measured
            let c: Catalog = serde_json::from_str(&json)?;
            measure(if hold { "shared" } else { "loaded" }, c, hold, heap);
        }
    }
    Ok(())
}
