//! Exports on two graphics cards (design: dual GPU, PR 3): a batch split over the second card
//! (the helper) and the editing card writes the same files as the editing card alone (within one
//! level per channel, decision 6), every file names the card that made it, the export keeps its
//! one activity row, the helper lost in the middle of a batch costs no file (that photo renders on
//! the processor, the rest on the editing card), and a cancel with photos on the helper returns at
//! once and gives the helper's turns back. On a one-card machine both devices are on the same card
//! (`set_helper("same")`). Its own process: it changes the devices every export renders on.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightcraft_engine::Session;
use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::export::{Destination, ExportCards, ExportOptions, prepare_batch, run_batch_cancellable, set_export_cards, set_export_lanes};
use lightcraft_engine::gpu::{self, Role};
use serde_json::{Value, json};

/// The tests change the process's cards and export settings: one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// The second device ready (on the second card when `LIGHTCRAFT_GPU_HELPER` names one, else on
/// the editing card); `false` (skipped) without a GPU.
fn helper_on() -> bool {
    if !gpu::available() {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter ({})", gpu::unavailable_reason().unwrap_or_default());
        return false;
    }
    if std::env::var_os("LIGHTCRAFT_GPU_HELPER").is_none() {
        gpu::set_helper("same").expect("same");
    }
    assert!(gpu::wait_helper(Duration::from_secs(240)), "the second device's set-up didn't finish");
    if !gpu::helper_ready() {
        lightcraft_testkit::skip!(Gpu, "no second device: {:?}", gpu::helper_status());
        return false;
    }
    eprintln!("editing card: {:?}, second: {:?}", gpu::card_in_use(), gpu::helper_status().name);
    true
}

/// A demo session whose first `n` photos carry a wedding-style edit, and the batch of `len` of them.
fn session(n: usize, len: usize, hdr: bool) -> (Session, Vec<PhotoId>) {
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).take(n).collect();
    for &id in &ids {
        s.set_develop(id, lightcraft_engine::machine_test::bench_settings(false, hdr), "test").unwrap();
    }
    let batch = ids.iter().copied().cycle().take(len).collect();
    (s, batch)
}

/// What one export batch left: the result entries, the files written, and the most renders the
/// second card held at once while it ran (sampled between photos).
struct Run {
    files: Vec<Value>,
    written: Vec<(String, Vec<u8>)>,
    helper_jobs: usize,
    rows: Vec<Vec<&'static str>>,
}

fn export(s: &mut Session, ids: &[PhotoId], o: &ExportOptions, cards: ExportCards, lanes: usize, cancel: Option<Arc<AtomicBool>>) -> Run {
    set_export_cards(cards);
    set_export_lanes(Some(lanes));
    for &id in ids {
        s.media.forget(id);
    }
    let items = prepare_batch(s, ids, o).unwrap();
    // the export's one row, as the app shows it (`export_task.rs`)
    let flag = cancel.clone().unwrap_or_default();
    let row = s.activity.start("export", "Exporting", lightcraft_engine::activity::Cancel::Flag(flag));
    let activity = s.activity.clone();
    let mut written = Vec::new();
    let mut helper_jobs = 0;
    let mut rows = Vec::new();
    let files = {
        let mut write = |p: &str, b: &[u8]| {
            written.push((p.to_string(), b.to_vec()));
            Ok(())
        };
        let c = cancel.clone();
        let mut progress = |_: usize, _: &str| {
            helper_jobs = helper_jobs.max(gpu::batch_usage_of(Role::Helper).jobs);
            rows.push(activity.list().iter().map(|t| t.kind).collect());
            !c.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
        };
        run_batch_cancellable(items, o, &Destination::default(), &mut write, &|_| false, false, &mut progress, cancel).unwrap()
    };
    drop(row);
    set_export_cards(ExportCards::Both);
    set_export_lanes(None);
    Run { files, written, helper_jobs, rows }
}

/// (mean, max) difference of the colour channels of two encoded files.
fn diff(a: &[u8], b: &[u8]) -> (f64, u8) {
    let (a, b) = (lightcraft_codecs::decode(a, Default::default()).unwrap().image, lightcraft_codecs::decode(b, Default::default()).unwrap().image);
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut max, mut n) = (0f64, 0f32, 0usize);
    for (p, q) in a.data.iter().zip(&b.data) {
        for c in 0..3 {
            let d = (p[c] - q[c]).abs() * 255.0;
            sum += d as f64;
            max = max.max(d);
            n += 1;
        }
    }
    (sum / n.max(1) as f64, max.round() as u8)
}

fn no_errors(r: &Run) {
    assert!(r.files.iter().all(|f| f.get("error").is_none()), "{:?}", r.files);
}

#[test]
fn a_batch_on_both_cards_writes_the_files_of_one_card() {
    let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !helper_on() {
        return;
    }
    let (mut s, ids) = session(4, 6, false);
    let o = ExportOptions::from_json(&json!({"format": "png", "longEdge": 1600}));
    let one = export(&mut s, &ids, &o, ExportCards::Editing, 3, None);
    let helper = export(&mut s, &ids, &o, ExportCards::Helper, 3, None);
    let both = export(&mut s, &ids, &o, ExportCards::Both, 4, None);
    for (what, r) in [("editing card", &one), ("second card", &helper), ("both cards", &both)] {
        no_errors(r);
        assert_eq!(r.written.len(), ids.len(), "{what}: every file written");
        // every photo rendered on a card, and says which
        for f in &r.files {
            assert!(f.get("gpuFallback").is_none(), "{what}: {f}");
            assert!(f.get("gpuDevice").and_then(Value::as_str).is_some_and(|c| !c.is_empty()), "{what}: no card named: {f}");
        }
        // the export keeps its one row: the cards are a resource, not a task
        assert!(r.rows.iter().all(|k| k == &["export"]), "{what}: {:?}", r.rows);
    }
    assert!(helper.helper_jobs > 0, "the second card took photos");
    for (other, what) in [(&helper, "second card"), (&both, "both cards")] {
        let (mut worst_mean, mut worst_max) = (0f64, 0u8);
        for ((pa, a), (pb, b)) in one.written.iter().zip(&other.written) {
            assert_eq!(pa, pb, "the same paths in the same order");
            let (mean, max) = diff(a, b);
            worst_mean = worst_mean.max(mean);
            worst_max = worst_max.max(max);
        }
        eprintln!("{what} vs editing card alone: worst mean {worst_mean:.4}, max {worst_max} levels");
        assert!(worst_max <= 1, "{what}: {worst_max} levels apart (decision 6 allows 1)");
    }
}

#[test]
fn the_second_card_lost_mid_batch_costs_no_file() {
    let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !helper_on() {
        return;
    }
    // one photo at a time, so exactly one is on the second card when it goes
    let (mut s, ids) = session(4, 6, true);
    let o = ExportOptions::from_json(&json!({"longEdge": 3000, "hdr": true, "quality": 90}));
    let done = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicUsize::new(0));
    let (d, l) = (done.clone(), lost.clone());
    let watcher = std::thread::spawn(move || {
        let t = Instant::now();
        while !d.load(Ordering::Relaxed) && t.elapsed() < Duration::from_secs(120) {
            if gpu::batch_usage_of(Role::Helper).jobs > 0 {
                // (let the render get going on the card)
                std::thread::sleep(Duration::from_millis(30));
                gpu::with_role(Role::Helper, || gpu::simulate_device_loss("device lost (Unknown): simulated driver reset"));
                l.store(1, Ordering::Relaxed);
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let r = export(&mut s, &ids, &o, ExportCards::Both, 1, None);
    done.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    assert_eq!(lost.load(Ordering::Relaxed), 1, "the second card took a photo and was lost");
    no_errors(&r);
    assert_eq!(r.written.len(), ids.len(), "every file written: {:?}", r.files);
    let fell_back: Vec<_> = r.files.iter().filter_map(|f| f.get("gpuFallback")).collect();
    eprintln!("second card lost mid-batch: {} fallback(s) {fell_back:?}", fell_back.len());
    assert!(fell_back.len() <= 1, "only the photo on the lost card renders on the processor: {fell_back:?}");
    // each file came from a card or, the one on the lost card, from the processor
    for f in &r.files {
        assert!(f.get("gpuDevice").is_some() || f.get("gpuFallback").is_some(), "{f}");
    }
    // the editing card kept rendering throughout
    assert!(gpu::with_role(Role::Primary, gpu::available), "the editing card still renders");
    // every file is a whole JPEG of the right size
    for (p, b) in &r.written {
        let img = lightcraft_codecs::decode(b, Default::default()).unwrap().image;
        assert_eq!(img.width.max(img.height), 3000, "{p}");
    }
}

#[test]
fn a_cancel_with_photos_on_the_second_card_returns_at_once() {
    let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !helper_on() {
        return;
    }
    let (mut s, ids) = session(4, 12, true);
    let o = ExportOptions::from_json(&json!({"longEdge": 1600, "hdr": true}));
    let cancel = Arc::new(AtomicBool::new(false));
    let at = Arc::new(Mutex::new(None));
    let (flag, when) = (cancel.clone(), at.clone());
    std::thread::spawn(move || {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(60) {
            if gpu::batch_usage_of(Role::Helper).jobs > 0 {
                *when.lock().unwrap() = Some(Instant::now());
                flag.store(true, Ordering::Relaxed);
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let r = export(&mut s, &ids, &o, ExportCards::Helper, 3, Some(cancel.clone()));
    let after = at.lock().unwrap().expect("a photo reached the second card").elapsed();
    assert!(after < Duration::from_millis(1500), "returned {after:?} after the cancel");
    assert!(r.files.len() < ids.len() && r.written.len() == r.files.len(), "{} files, {} written", r.files.len(), r.written.len());
    // the lanes still in flight stop at their next step and give the second card's turns back
    let t = Instant::now();
    while gpu::batch_usage_of(Role::Helper).jobs > 0 && t.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(gpu::batch_usage_of(Role::Helper).jobs, 0, "the second card's turns were given back");
}
