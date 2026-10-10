//! Catalog scale benchmark (`cargo xtask bench-catalog --photos 250000,1000000`).
//!
//! For each size: build a synthetic library (import throughput: ops applied + appended in batches
//! of 1000, as an import does), compact it (snapshot time), then reopen it **in a fresh process**
//! so its peak RSS is the open's alone, and time the filter bar's queries on it.
//!
//! ```text
//! bench_catalog [--backend v3|v4] [--dir DIR] [--photos N,N…]
//! bench_catalog --phase open --backend v3|v4 --dir DIR      (child: prints one JSON line)
//! ```
//!
//! Output: one JSON line per size (`{"backend":…,"photos":…,"import_per_s":…,…}`), then a table.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use dac_catalog::*;
use dac_develop::DevelopSettings;

type R<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

const CAMERAS: [&str; 6] = ["Camera A1", "Camera B2", "Camera C3", "Phone X", "Camera D4", "Camera E5"];
const KEYWORDS: [&str; 8] = ["travel", "family", "travel|italy", "birds", "street", "portrait", "landscape", "night"];

/// A plausible library photo: 1 in 5 edited (with two history steps), ratings, flags, labels,
/// keywords, a capture date spread over 15 years.
fn photo(c: &mut Catalog, i: u64) -> Photo {
    let id = c.alloc_photo_id();
    let year = 2010 + (i % 15);
    let mut p = Photo::new(
        id,
        Source::File { path: format!("/home/someone/Pictures/{year}/{:05}/IMG_{i:07}.CR3", i / 400) },
        &format!("IMG_{i:07}.CR3"),
        "CR3",
        6000,
        4000,
        "2026-09-30T12:00:00",
    );
    p.kind = if i.is_multiple_of(50) { MediaKind::Video } else { MediaKind::Raw };
    p.file_size = 25_000_000 + i;
    p.captured = Some(format!("{year}-{:02}-{:02}T10:{:02}:{:02}", 1 + i % 12, 1 + i % 28, i % 60, (i / 60) % 60));
    p.content_hash = Some(format!("{:032x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
    p.meta.camera = CAMERAS[(i % 6) as usize].into();
    p.meta.lens = "24-70mm f/2.8".into();
    p.meta.focal_mm = Some(35.0);
    p.meta.aperture = Some(4.0);
    p.meta.shutter = "1/250".into();
    p.meta.iso = Some(100 << (i % 6));
    p.meta.keywords = vec![KEYWORDS[(i % 8) as usize].into()];
    p.as_shot_wb = Some((5200.0, 3.0));
    p.rating = (i % 6) as u8;
    p.flag = match i % 7 {
        0 => Flag::Pick,
        1 => Flag::Reject,
        _ => Flag::None,
    };
    p.label = if i.is_multiple_of(9) { Some(ColorLabel::Red) } else { None };
    if i.is_multiple_of(5) {
        let mut d = DevelopSettings::default();
        d.light.exposure = (i % 7) as f64 * 0.1;
        let s = Arc::new(d);
        p.develop = s.clone();
        p.edited = Some("2026-09-30T12:00:00".into());
        p.history = vec![
            HistoryStep { label: "Import".into(), settings: Arc::new(DevelopSettings::default()) },
            HistoryStep { label: "Exposure".into(), settings: s },
        ];
    }
    p
}

/// The filter bar's typical queries.
fn filters() -> Vec<(&'static str, Filter)> {
    vec![
        ("all", Filter::default()),
        ("rating>=4", Filter { rating: 4, ..Default::default() }),
        ("flag=pick", Filter { flag: Some(Flag::Pick), ..Default::default() }),
        ("camera", Filter { camera: Some("Camera C3".into()), ..Default::default() }),
        ("keyword", Filter { keyword: Some("travel".into()), ..Default::default() }),
        ("date=2019", Filter { date: Some("2019".into()), ..Default::default() }),
        ("text", Filter { text: "IMG_00123".into(), ..Default::default() }),
    ]
}

/// Peak resident set size of this process (MB), where the OS says it.
fn peak_rss_mb() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map(|rd| rd.filter_map(|e| e.ok()?.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()).unwrap_or(0)
}

fn open_journal(backend: &str, dir: &Path) -> R<(Journal, Catalog)> {
    let (j, c, _) = match backend {
        "v4" => return Err("the v4 backend is not built yet".into()),
        _ => Journal::open(Box::new(FsStore::open(dir)?))?,
    };
    Ok((j, c))
}

/// Build `n` photos into `dir`: returns (import photos/s, snapshot ms, bytes on disk).
fn build(backend: &str, dir: &Path, n: u64) -> R<(f64, f64, u64)> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let (mut j, mut cat) = open_journal(backend, dir)?;
    let t = Instant::now();
    let mut i = 0u64;
    while i < n {
        let end = (i + 1000).min(n);
        let ops: Vec<Op> = (i..end).map(|k| Op::AddPhoto { photo: Box::new(photo(&mut cat, k)) }).collect();
        for op in &ops {
            cat.apply(op.clone())?;
        }
        j.append(&ops)?;
        i = end;
    }
    let import_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    j.snapshot(&cat)?;
    let snap_ms = ms(t);
    drop(j);
    Ok((n as f64 / import_s.max(1e-9), snap_ms, dir_bytes(dir)))
}

/// Child process: open, query, print one JSON line.
fn open_phase(backend: &str, dir: &Path) -> R<()> {
    let t = Instant::now();
    let (_j, cat) = open_journal(backend, dir)?;
    let open_ms = ms(t);
    let mut q = serde_json::Map::new();
    let sort = Sort::default();
    for (name, f) in filters() {
        // the first run warms caches; the median of 3 is reported
        let mut v: Vec<f64> = (0..3)
            .map(|_| {
                let t = Instant::now();
                let ids = cat.query(&f, &sort);
                std::hint::black_box(ids.len());
                ms(t)
            })
            .collect();
        v.sort_by(f64::total_cmp);
        q.insert(name.into(), serde_json::json!(v.get(1).copied().unwrap_or(0.0)));
    }
    println!("{}", serde_json::json!({"photos": cat.len(), "open_ms": open_ms, "peak_rss_mb": peak_rss_mb(), "filter_ms": q}));
    Ok(())
}

fn main() -> R<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let backend = arg("--backend").unwrap_or_else(|| "v3".into());
    let dir = PathBuf::from(arg("--dir").unwrap_or_else(|| "target/bench-catalog".into()));
    if arg("--phase").as_deref() == Some("open") {
        return open_phase(&backend, &dir);
    }
    let sizes: Vec<u64> =
        arg("--photos").unwrap_or_else(|| "250000,1000000".into()).split(',').map(|s| s.trim().parse()).collect::<std::result::Result<_, _>>()?;
    let mut rows = Vec::new();
    for n in sizes {
        let d = dir.join(format!("{backend}-{n}"));
        eprintln!("building {n} photos ({backend}) in {}", d.display());
        let (import_per_s, snapshot_ms, bytes) = build(&backend, &d, n)?;
        let out = std::process::Command::new(std::env::current_exe()?).args(["--phase", "open", "--backend", &backend, "--dir"]).arg(&d).output()?;
        if !out.status.success() {
            return Err(format!("open phase failed: {}", String::from_utf8_lossy(&out.stderr)).into());
        }
        let mut v: serde_json::Value = serde_json::from_slice(out.stdout.trim_ascii())?;
        if let Some(o) = v.as_object_mut() {
            o.insert("backend".into(), backend.clone().into());
            o.insert("import_per_s".into(), import_per_s.round().into());
            o.insert("snapshot_ms".into(), snapshot_ms.round().into());
            o.insert("disk_mb".into(), ((bytes as f64 / 1e5).round() / 10.0).into());
        }
        println!("{v}");
        rows.push(v);
        let _ = std::fs::remove_dir_all(&d);
    }
    println!("\n| backend | photos | open | peak RSS | filter (worst) | snapshot | import | disk |\n|---|---|---|---|---|---|---|---|");
    for v in rows {
        let worst = v["filter_ms"].as_object().map(|o| o.values().filter_map(|x| x.as_f64()).fold(0.0, f64::max)).unwrap_or(0.0);
        println!(
            "| {} | {} | {:.0} ms | {:.0} MB | {:.0} ms | {} ms | {}/s | {} MB |",
            v["backend"].as_str().unwrap_or("?"),
            v["photos"],
            v["open_ms"].as_f64().unwrap_or(0.0),
            v["peak_rss_mb"].as_f64().unwrap_or(0.0),
            worst,
            v["snapshot_ms"],
            v["import_per_s"],
            v["disk_mb"]
        );
    }
    Ok(())
}
