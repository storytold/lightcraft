//! Catalog persistence costs on large synthetic catalogs (issue #37): an import that compacts as
//! the app does (how often, and how much it writes in all), log append, compaction (snapshot
//! serialise / write+fsync / log reset), synchronous and on the background worker, load, and a
//! catalog clone.
//!
//! ```text
//! cargo run --release -p lightcraft-catalog --example persist_bench -- <dir> [photos…]
//! ```
//!
//! `<dir>` is a scratch directory (wiped per size). Default sizes: 1000 10000 85000. 90 % of the
//! photos are Local browse records, the rest library photos with a few edits and history steps;
//! `PERSIST_BENCH_MIX=library` makes them all unedited JPEGs of a library, with a title;
//! `PERSIST_BENCH_MIX=edited` makes them all edited raws with 20 history steps each.
//! Peak memory: run under `/usr/bin/time -l` (macOS) / `-v` (Linux) with one size;
//! `PERSIST_BENCH_NO_LOAD=1` skips the final load, so the peak is building + compacting.

use std::sync::Arc;
use std::time::Instant;

use lightcraft_catalog::*;
use lightcraft_develop::DevelopSettings;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// `PERSIST_BENCH_MIX=library`: an imported JPEG nobody edited.
fn library_photo(c: &mut Catalog, i: u64) -> Photo {
    let id = c.alloc_photo_id();
    let name = format!("IMG_{i:08}.jpg");
    let path = format!("/Volumes/Photos/Library/Archive/2019/2019-07 Summer/Trip to the coast/Day {:05}/extra/{name}", i / 500);
    let mut p = Photo::new(id, Source::File { path }, &name, "JPEG", 4032, 3024, "2026-09-30T12:00:00");
    p.file_size = 3_400_000 + i;
    p.captured = Some(format!("2019-07-{:02}T10:{:02}:{:02}", 1 + i % 28, i % 60, (i / 60) % 60));
    p.content_hash = Some(format!("{:032x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
    p.meta.title = format!("IMG_{i:07}");
    p
}

/// `PERSIST_BENCH_MIX=edited`: a raw with 20 edits, each a history step with its own settings.
fn edited_photo(c: &mut Catalog, i: u64) -> Photo {
    let mut p = library_photo(c, i);
    p.kind = MediaKind::Raw;
    p.format = "CR3".into();
    p.as_shot_wb = Some((5200.0, 3.0));
    let mut d = p.import_defaults();
    for step in 0..20u64 {
        match step % 4 {
            0 => d.light.exposure = (i % 7) as f64 * 0.1 + step as f64 * 0.01,
            1 => d.light.contrast = step as f64,
            2 => d.wb.tint = (i % 11) as f64 + step as f64,
            _ => d.detail.sharpen_amount = 40.0 + step as f64,
        }
        p.history.push(HistoryStep { label: "Edit".into(), settings: Arc::new(d.clone()) });
    }
    p.develop = p.history.last().map(|s| s.settings.clone()).unwrap_or_default();
    p.edited = Some("2026-09-30T12:00:00".into());
    p.rating = (i % 6) as u8;
    p
}

fn photo(c: &mut Catalog, i: u64) -> Photo {
    match std::env::var("PERSIST_BENCH_MIX").as_deref() {
        Ok("library") => return library_photo(c, i),
        Ok("edited") => return edited_photo(c, i),
        _ => {}
    }
    let id = c.alloc_photo_id();
    let mut p = Photo::new(
        id,
        Source::File { path: format!("/Users/someone/Pictures/2026/09/{:05}/IMG_{i:05}.CR3", i / 500) },
        &format!("IMG_{i:05}.CR3"),
        "CR3",
        6000,
        4000,
        "2026-09-30T12:00:00",
    );
    p.kind = MediaKind::Raw;
    p.file_size = 25_000_000 + i;
    p.captured = Some(format!("2026-09-{:02}T10:{:02}:{:02}", 1 + i % 28, i % 60, (i / 60) % 60));
    p.content_hash = Some(format!("{:032x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
    p.meta.camera = "Camera Model X".into();
    p.meta.lens = "24-70mm f/2.8".into();
    p.meta.focal_mm = Some(35.0);
    p.meta.aperture = Some(4.0);
    p.meta.shutter = "1/250".into();
    p.meta.iso = Some(400);
    p.as_shot_wb = Some((5200.0, 3.0));
    p.local = !i.is_multiple_of(10);
    if p.local {
        // as a browse catalogues it: import defaults, a baseline fingerprint
        p.develop = Arc::new(p.import_defaults());
        p.set_local_baseline();
    }
    if !p.local {
        let mut d = DevelopSettings::default();
        d.light.exposure = (i % 7) as f64 * 0.1;
        let s = Arc::new(d);
        p.develop = s.clone();
        p.edited = Some("2026-09-30T12:00:00".into());
        p.history = vec![HistoryStep { label: "Exposure".into(), settings: s }];
        p.rating = (i % 6) as u8;
    }
    p
}

/// Log rating changes (2000 per append) until the journal asks for a compaction.
fn fill_until_due(j: &mut Journal, cat: &mut Catalog, ids: &[PhotoId]) -> std::result::Result<(), Box<dyn std::error::Error>> {
    while !j.wants_snapshot() {
        let ops: Vec<Op> = (0..2000u64)
            .map(|k| Op::SetRating { id: ids.get((k as usize) % ids.len().max(1)).copied().unwrap_or(PhotoId(1)), rating: ((j.seq() + k) % 6) as u8 })
            .collect();
        for op in &ops {
            cat.apply(op.clone())?;
        }
        j.append(&ops)?;
    }
    Ok(())
}

fn stats(mut v: Vec<f64>) -> String {
    v.sort_by(f64::total_cmp);
    let pick = |q: f64| v.get(((v.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
    format!("median {:.2} ms, p95 {:.2} ms, max {:.2} ms", pick(0.5), pick(0.95), pick(1.0))
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().ok_or("usage: persist_bench <scratch dir> [photos…]")?);
    let mut sizes: Vec<u64> = args.map(|a| a.parse()).collect::<std::result::Result<_, _>>()?;
    if sizes.is_empty() {
        sizes = vec![1000, 10_000, 85_000];
    }
    for n in sizes {
        let _ = std::fs::remove_dir_all(&dir);
        let (mut j, _, _) = Journal::open(Box::new(FsStore::open(&dir)?))?;
        let mut cat = Catalog::new();
        // an import: photos added one op each, the log compacted whenever the journal asks for
        // it (as the app does; synchronously here, so the numbers don't depend on timing)
        let t = Instant::now();
        let (mut compactions, mut written, mut log_written) = (0u64, 0u64, 0u64);
        for chunk in (0..n).collect::<Vec<_>>().chunks(500) {
            let ops: Vec<Op> = chunk.iter().map(|i| Op::AddPhoto { photo: Box::new(photo(&mut cat, *i)) }).collect();
            for op in &ops {
                cat.apply(op.clone())?;
            }
            let before = j.log_bytes();
            j.append(&ops)?;
            log_written += j.log_bytes() - before;
            if j.wants_snapshot() {
                j.snapshot(&cat)?;
                compactions += 1;
                written += j.stats().last_snapshot.bytes;
            }
        }
        let log_left = j.log_bytes();
        // (a last batch that reached a threshold was snapshotted already: not again)
        if log_left > 0 {
            j.snapshot(&cat)?;
            written += j.stats().last_snapshot.bytes;
        }
        let import_ms = ms(t);
        let first = j.stats().last_snapshot;
        println!(
            "== {n} photos ({} local): snapshot {:.1} MB ({:.0} B per photo)",
            cat.photos().filter(|p| p.local).count(),
            first.bytes as f64 / 1e6,
            first.bytes as f64 / n.max(1) as f64
        );
        println!(
            "  import: {compactions} compactions wrote {:.1} MB of snapshots ({:.1}x the final one) + {:.1} MB of log in {:.1} s; {:.1} MB of log left at the end",
            written as f64 / 1e6,
            written as f64 / first.bytes.max(1) as f64,
            log_written as f64 / 1e6,
            import_ms / 1e3,
            log_left as f64 / 1e6
        );
        if std::env::var_os("PERSIST_BENCH_NO_LOAD").is_none() {
            let t = Instant::now();
            let (_, loaded, _) = Journal::open(Box::new(FsStore::open(&dir)?))?;
            println!("  open (the whole catalog from its snapshot): {:.1} ms", ms(t));
            if loaded.len() != cat.len() {
                return Err("reloaded catalog differs".into());
            }
        }

        let t = Instant::now();
        let copy = cat.clone();
        println!("  catalog clone: {:.2} ms", ms(t));
        drop(copy);

        // single-op appends (a rating change each), as commands do
        let ids: Vec<PhotoId> = cat.photos().map(|p| p.id).take(1000).collect();
        let mut appends = Vec::new();
        for k in 0..300usize {
            let id = ids.get(k % ids.len().max(1)).copied().unwrap_or(PhotoId(1));
            let op = Op::SetRating { id, rating: (k % 6) as u8 };
            cat.apply(op.clone())?;
            j.append(std::slice::from_ref(&op))?;
            appends.push(j.stats().last_append_ms);
        }
        println!("  append (1 op, fsync): {}", stats(appends));

        // compactions when the journal asks for them
        let mut totals = Vec::new();
        for _ in 0..3 {
            fill_until_due(&mut j, &mut cat, &ids)?;
            j.snapshot(&cat)?;
            let s = j.stats().last_snapshot;
            println!(
                "  compaction ({} records): serialize {:.1} ms, write+sync {:.1} ms, log reset {:.1} ms, total {:.1} ms",
                s.records, s.serialize_ms, s.write_sync_ms, s.reset_ms, s.total_ms
            );
            totals.push(s.total_ms);
        }
        println!("  compaction total: {}", stats(totals));

        // the same, written by the worker while single-op appends go on
        let mut blocking = Vec::new();
        for _ in 0..3 {
            fill_until_due(&mut j, &mut cat, &ids)?;
            let t = Instant::now();
            j.snapshot_in_background(&cat)?;
            let start_ms = ms(t);
            let mut during = Vec::new();
            while !j.snapshot_written() {
                let op = Op::SetRating { id: ids.get(1).copied().unwrap_or(PhotoId(1)), rating: (j.seq() % 6) as u8 };
                cat.apply(op.clone())?;
                j.append(&[op])?;
                during.push(j.stats().last_append_ms);
            }
            let t = Instant::now();
            j.poll()?;
            let finish_ms = ms(t);
            let s = j.stats().last_snapshot;
            println!(
                "  background compaction ({} records): start {start_ms:.1} ms + finish {finish_ms:.1} ms blocking; worker {:.1} ms; {} appends during it: {}",
                s.records,
                s.total_ms,
                during.len(),
                stats(during)
            );
            blocking.push(s.blocking_ms);
        }
        println!("  background compaction blocking: {}", stats(blocking));

        // forget the untouched Local records (folders last browsed long ago; every 7th folder
        // recently; every 50th Local record rated), then compact
        let folders: std::collections::BTreeSet<String> = cat.photos().filter(|p| p.local).filter_map(|p| folder_of(p)).collect();
        let mut ops: Vec<Op> = folders
            .iter()
            .enumerate()
            .map(|(k, f)| Op::SetBrowsed {
                folder: f.clone(),
                at: Some(if k % 7 == 0 { "2026-09-30T00:00:00" } else { "2026-06-01T00:00:00" }.into()),
            })
            .collect();
        ops.extend(cat.photos().filter(|p| p.local && p.id.0.is_multiple_of(50)).map(|p| Op::SetRating { id: p.id, rating: 2 }).collect::<Vec<_>>());
        for op in &ops {
            cat.apply(op.clone())?;
        }
        j.append(&ops)?;
        j.snapshot(&cat)?;
        let before = j.stats().last_snapshot.bytes;
        let t = Instant::now();
        let plan = cat.forget_local_plan("2026-10-05T00:00:00", DEFAULT_FORGET_DAYS, &|_| false);
        let plan_ms = ms(t);
        let t = Instant::now();
        let ops = plan.ops();
        for op in &ops {
            cat.apply(op.clone())?;
        }
        j.append(&ops)?;
        let apply_ms = ms(t);
        j.snapshot(&cat)?;
        let after = j.stats().last_snapshot.bytes;
        println!(
            "  forget Local: {} of {} local records forgotten (kept: {} recent, {} touched, {} in use); plan {plan_ms:.1} ms, apply+append {apply_ms:.1} ms; snapshot {:.1} MB -> {:.1} MB",
            plan.evict.len(),
            plan.local,
            plan.kept_recent,
            plan.kept_touched,
            plan.kept_in_use,
            before as f64 / 1e6,
            after as f64 / 1e6
        );

        drop(j);
        if std::env::var_os("PERSIST_BENCH_NO_LOAD").is_some() {
            // peak memory of building + compacting only
            continue;
        }
        let t = Instant::now();
        let (mut j, mut loaded, _) = Journal::open(Box::new(FsStore::open(&dir)?))?;
        let open_ms = ms(t);
        println!("  open (after forgetting; load snapshot + replay): {open_ms:.1} ms");
        if loaded.len() != cat.len() {
            return Err("reloaded catalog differs".into());
        }
        // a long log (never compacted here): what replaying it costs when the library is opened
        let ids: Vec<PhotoId> = loaded.photos().map(|p| p.id).take(1000).collect();
        const REPLAYED: u64 = 200_000;
        for batch in 0..REPLAYED / 2000 {
            let ops: Vec<Op> = (0..2000u64)
                .map(|k| Op::SetRating {
                    id: ids.get((k as usize) % ids.len().max(1)).copied().unwrap_or(PhotoId(1)),
                    rating: ((batch + k) % 6) as u8,
                })
                .collect();
            for op in &ops {
                loaded.apply(op.clone())?;
            }
            j.append(&ops)?;
        }
        let log_mb = j.log_bytes() as f64 / 1e6;
        drop(j);
        let t = Instant::now();
        let (_, _, report) = Journal::open(Box::new(FsStore::open(&dir)?))?;
        println!("  open with {} more log records ({log_mb:.1} MB): {:.1} ms ({:.1} ms more)", report.replayed, ms(t), ms(t) - open_ms);
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
