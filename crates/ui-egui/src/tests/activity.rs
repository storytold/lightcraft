//! Issue #345: the activity stack — one place, top-left, for every long-running task, with ✕ to cancel.

use std::time::Duration;

use lightcraft_engine::activity::{Cancel, TaskInfo, Unit};
use serde_json::json;

use crate::headless::Headless;
use crate::panels::activity::{WIDTH, count_text};
use crate::{LightcraftApp, Services};

// generous: merges and renders run on worker threads that a busy machine (a full workspace test run) slows a lot
const T: Duration = Duration::from_secs(60);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // let the first renders finish: a render still running when the test binary exits can crash in the
    // GPU driver's teardown (the preview pool doesn't join its workers)
    h.settle(T);
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn rows(h: &Headless) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with("activity:row:")).count()
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// Rows show once their task is half a second old.
fn wait_visible(h: &mut Headless) {
    std::thread::sleep(Duration::from_millis(600));
    h.step();
    h.step();
}

/// Click a widget and let the synthetic events (move, press, release) play out.
fn click(h: &mut Headless, id: &str) {
    h.step();
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    for _ in 0..4 {
        h.step();
    }
}

#[test]
fn rows_appear_after_half_a_second_and_cancel_by_click() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    g.progress(2, 5);
    h.step();
    // a frame on a loaded machine can outlast the half second: only judge a task still young after it
    if h.app.session.activity.list()[0].age_ms < crate::panels::activity::SHOW_AFTER_MS {
        assert!(!has(&h, &format!("activity:row:{}", g.id())), "not before 0.5 s");
    }
    wait_visible(&mut h);
    assert!(has(&h, &format!("activity:row:{}", g.id())));
    click(&mut h, &format!("activity:cancel:{}", g.id()));
    assert!(g.is_cancelled());
    let inspect = h.request("ui.inspect", json!({}), T);
    assert_eq!(inspect["result"]["activity"][0]["cancelling"], true, "{}", inspect["result"]["activity"]);
}

#[test]
fn more_than_three_rows_overflow_and_expand() {
    let mut h = demo();
    let guards: Vec<_> = (0..5).map(|i| h.app.session.activity.start("export", &format!("Task {i}"), Cancel::Yes)).collect();
    wait_visible(&mut h);
    assert_eq!(rows(&h), 3);
    assert!(has(&h, "activity:more"));
    click(&mut h, "activity:more");
    assert_eq!(rows(&h), 5);
    // expanded, the stack grows to show them all
    let stack = h.view.ctx.memory(|m| m.area_rect(egui::Id::unique("activity-stack"))).expect("the stack is drawn");
    for g in &guards {
        let r = rect(&h, &format!("activity:row:{}", g.id()));
        assert!(stack.contains_rect(r), "row {} at {r:?} is cut off by the stack at {stack:?}", g.id());
    }
    drop(guards);
    h.step();
    assert_eq!(rows(&h), 0);
}

#[test]
fn not_cancellable_rows_have_no_cross() {
    let mut h = demo();
    let g = h.app.session.activity.start("faces", "Finding faces", Cancel::No);
    wait_visible(&mut h);
    assert!(has(&h, &format!("activity:row:{}", g.id())));
    assert!(!has(&h, &format!("activity:cancel:{}", g.id())));
}

#[test]
fn count_text_by_unit() {
    let mut t = TaskInfo {
        id: 1,
        kind: "export",
        label: "Exporting".into(),
        done: 3,
        total: 25,
        unit: Unit::Count,
        detail: String::new(),
        cancellable: true,
        cancelling: false,
        age_ms: 600,
        stalled: None,
    };
    assert_eq!(count_text(&t), "3 of 25");
    (t.done, t.total, t.unit) = (12 * 1_048_576, 340 * 1_048_576, Unit::Bytes);
    assert_eq!(count_text(&t), "12 of 340 MB");
    (t.done, t.total, t.unit) = (400, 1000, Unit::Percent);
    assert_eq!(count_text(&t), "40 %");
    t.total = 0;
    assert_eq!(count_text(&t), "");
}

#[test]
fn stale_cancel_is_harmless() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    let id = g.id();
    drop(g);
    let r = h.request("engine.execute", json!({"command": "activity.cancel", "params": {"id": id}}), T);
    assert_eq!(r["ok"], false, "an error result, not a panic: {r}");
    h.step();
    assert!(h.app.ui.toast.is_none(), "{:?}", h.app.ui.toast);
    // the same through the row's ✕ (clicked in the frame the task ended): no message in the status bar either
    h.app.ui.status = "Ready".into();
    crate::panels::activity::cancel(&mut h.app, id);
    h.step();
    assert_eq!(h.app.ui.status, "Ready");
    assert!(h.app.ui.toast.is_none(), "{:?}", h.app.ui.toast);
}

#[test]
fn long_detail_stays_inside_the_stack() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exportieren", Cancel::Yes);
    g.detail(&"IMG_".repeat(75));
    wait_visible(&mut h);
    let row = rect(&h, &format!("activity:row:{}", g.id()));
    assert!(row.width() <= WIDTH + 1.0, "{row:?}");
}

#[test]
fn cross_sits_at_the_right_edge_of_its_row() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    let row = rect(&h, &format!("activity:row:{}", g.id()));
    let cross = rect(&h, &format!("activity:cancel:{}", g.id()));
    assert!(cross.right() >= row.right() - 2.0, "the cross is at the right, not after the label: {cross:?} in {row:?}");
}

#[test]
fn quit_with_a_running_task_asks_and_quit_anyway_cancels() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    assert!(!crate::panels::notices::may_close(&mut h.app));
    assert!(matches!(h.app.quit_prompt, Some(crate::QuitPrompt::Tasks(_))), "{:?}", h.app.quit_prompt);
    h.step();
    assert!(!has(&h, "button:quitRetry"));
    click(&mut h, "button:quitAnyway");
    assert!(g.is_cancelled() && h.quit_requested());
}

#[test]
fn quit_cancel_keeps_the_task_running() {
    let mut h = demo();
    let g = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    assert!(!crate::panels::notices::may_close(&mut h.app));
    click(&mut h, "button:quitCancel");
    assert!(!g.is_cancelled() && h.app.quit_prompt.is_none() && !h.app.quit_confirmed);
}

#[test]
fn cancelling_task_does_not_block_quit() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    h.app.session.activity.cancel(g.id()).unwrap();
    let _f = h.app.session.activity.start("faces", "Finding faces", Cancel::No);
    assert!(crate::panels::notices::may_close(&mut h.app));
}

/// A slow disk: every file takes `ms` to write (the UI must not wait for it).
fn slow_writer(ms: u64) -> crate::SharedWrite {
    std::sync::Arc::new(move |_: &str, _: &[u8]| {
        std::thread::sleep(Duration::from_millis(ms));
        Ok(())
    })
}

/// Export the first `n` photos in view through the Export dialog (small JPEGs, a made-up folder).
fn start_export(h: &mut Headless, n: usize) {
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(n).map(|p| p.0).collect();
    assert_eq!(ids.len(), n, "the demo library has {n} photos");
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids}}), T);
    h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), T);
    h.step();
    if let Some(crate::state::Dialog::Export { full_size, resize, dir, .. }) = &mut h.app.ui.dialog {
        *full_size = false;
        *resize = lightcraft_engine::export::Resize::long_edge(64);
        *dir = "/lc-test-out".into();
    }
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
}

#[test]
fn export_shows_a_row_and_stops_on_activity_cancel() {
    let mut h = demo();
    h.app.services.write_shared = Some(slow_writer(200));
    start_export(&mut h, 5);
    assert!(h.app.export.is_some(), "running in the background");
    let tasks = h.request("ui.inspect", json!({}), T)["result"]["activity"].clone();
    assert_eq!(tasks[0]["kind"], "export", "{tasks}");
    assert_eq!(tasks[0]["total"], 5, "{tasks}");
    let id = tasks[0]["id"].as_u64().unwrap();
    assert_eq!(h.request("engine.execute", json!({"command": "activity.cancel", "params": {"id": id}}), T)["ok"], true);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
    assert!(h.app.session.activity.list().is_empty());
    assert!(h.app.last_export_result.as_ref().is_some_and(|r| r["cancelled"] == true), "{:?}", h.app.last_export_result);
    assert!(!has(&h, "button:exportCancel"), "the old window is gone");
}

#[test]
fn dead_export_worker_leaves_no_row() {
    let mut h = demo();
    h.app.services.write_shared = Some(std::sync::Arc::new(|_: &str, _: &[u8]| -> Result<(), String> { panic!("synthetic writer panic") }));
    start_export(&mut h, 2);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
    assert!(h.app.session.activity.list().is_empty());
}

/// A folder of `n` stand-in JPEGs and an app whose file probe takes 100 ms each (a slow drive).
fn slow_import(tag: &str, n: usize) -> (Headless, std::path::PathBuf) {
    gated_import(tag, n, None)
}

/// Like `slow_import`, and while `gate` is false the probe holds every file after the first batch
/// (`batch_size()` files, which join the catalog together), so a test can act mid-import without
/// racing the 100 ms-per-file clock. The hold gives up after 60 s, so a failed test never leaves
/// the import thread spinning.
fn gated_import(tag: &str, n: usize, gate: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> (Headless, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-activity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..n {
        std::fs::write(dir.join(format!("IMG_{i:03}.jpg")), format!("not really a jpeg {i}")).unwrap();
    }
    let mut s = lightcraft_engine::Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(move |p: &str| {
        std::thread::sleep(Duration::from_millis(100));
        let index = p.rsplit_once("IMG_").and_then(|(_, n)| n.strip_suffix(".jpg")).and_then(|n| n.parse::<usize>().ok());
        if let (Some(gate), Some(index)) = (&gate, index)
            && index >= lightcraft_engine::import::batch_size()
        {
            let t0 = std::time::Instant::now();
            while !gate.load(std::sync::atomic::Ordering::Acquire) && t0.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(lightcraft_engine::media::ProbeInfo {
            width: 60,
            height: 40,
            format: "JPEG".into(),
            content_hash: Some(p.to_string()),
            ..Default::default()
        })
    }));
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    (h, dir)
}

#[test]
fn import_cancelled_by_command_reports_cancelled() {
    let n = 40;
    let (mut h, dir) = slow_import("cancel", n);
    let undo0 = h.app.session.undo.len();
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.as_ref().is_some_and(|t| t.imported >= 2)));
    let tasks = h.app.session.activity.list();
    assert_eq!(tasks.first().map(|t| t.kind), Some("import"), "{tasks:?}");
    h.app.session.activity.cancel(tasks[0].id).unwrap();
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.is_none()));
    assert!(h.app.session.activity.list().is_empty());
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Import cancelled")), "{:?}", h.app.ui.toast);
    assert!(h.app.session.catalog.len() < n, "stopped part-way");
    assert_eq!(h.app.session.undo.len(), undo0 + 1, "one undo step");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A photo chosen while an import runs stays the active one when the import ends: the import
/// selects its first photo only if nobody picked something else meanwhile (issue #1446).
#[test]
fn a_photo_chosen_during_an_import_stays_selected() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut h, dir) = gated_import("chosen", 20, Some(gate.clone()));
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.as_ref().is_some_and(|t| t.imported >= 2)));
    // the newest photo so far, never the import's first
    let chosen = h.app.session.catalog.photos().map(|p| p.id).max().expect("photos arrived");
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [chosen.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    // held at the gate until now, so the import cannot have ended however slowly the frames ran
    assert!(h.app.import.is_some(), "still importing");
    gate.store(true, std::sync::atomic::Ordering::Release);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.is_none()));
    assert_eq!(h.app.session.catalog.len(), 20, "all imported");
    assert_eq!(h.app.session.selection.active, Some(chosen));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_shows_a_row_and_cancel_closes_it() {
    let (mut h, dir) = slow_import("scan", 120);
    crate::import::open(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    h.step();
    let tasks = h.app.session.activity.list();
    assert_eq!(tasks.first().map(|t| t.kind), Some("scan"), "{tasks:?}");
    assert!(tasks[0].cancellable);
    h.app.session.activity.cancel(tasks[0].id).unwrap();
    // like the old Cancel: the scan is dropped at once, without waiting for the file being read
    assert!(h.step_until(Duration::from_secs(10), |h| h.app.scan.is_none()));
    assert!(h.app.session.activity.list().is_empty());
    for _ in 0..3 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none(), "no review opened");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library of three bracketed DNGs (−2, 0, +2 EV) on disk, all selected.
fn bracket(tag: &str) -> (Headless, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-activity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for (i, bytes) in lightcraft_merge::synth::bracket_dngs(1600, 1200, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, bytes).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut session = lightcraft_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    session.execute("library.select", &json!({"ids": ids})).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    (h, dir)
}

#[test]
fn final_merge_shows_a_row_and_cancels() {
    let (mut h, dir) = bracket("merge");
    let before = h.app.session.catalog.len();
    crate::merge::start_last(&mut h.app, "merge.hdr").unwrap();
    h.step();
    let rows = h.app.session.activity.list();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].unit), ("merge", "Merging", Unit::Percent));
    assert!(rows[0].cancellable);
    assert_eq!(rows[0].total, 1000);
    h.app.session.activity.cancel(rows[0].id).unwrap();
    assert!(h.step_until(T, |h| h.app.merge.final_task.is_none()));
    assert!(h.app.merge.last_result.is_none(), "nothing merged");
    assert_eq!(h.app.session.catalog.len(), before, "no photo added");
    assert!(h.app.session.activity.list().is_empty());
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert_eq!(toast, "HDR merge cancelled");
    let _ = std::fs::remove_dir_all(dir);
}

/// The merge dialog opens with the options of the last merge of its kind (per kind), and "with Last
/// Settings" uses them; a kind never merged opens with the defaults.
#[test]
fn merge_dialog_remembers_the_last_settings_per_kind() {
    let (mut h, dir) = bracket("merge-last");
    let opts = crate::merge::MergeDialog {
        align: false,
        deghost: "high".into(),
        auto_settings: false,
        ..crate::merge::MergeDialog::for_command("merge.hdr")
    };
    crate::merge::open(&mut h.app, "merge.hdr").unwrap();
    h.app.ui.dialog = None;
    crate::merge::start_final(&mut h.app, &opts).unwrap();
    let row = h.app.session.activity.list()[0].id;
    h.app.session.activity.cancel(row).unwrap();
    assert!(h.step_until(T, |h| h.app.merge.final_task.is_none()));
    crate::merge::open(&mut h.app, "merge.hdr").unwrap();
    let Some(crate::state::Dialog::Merge { opts: shown }) = &h.app.ui.dialog else { panic!("no merge dialog") };
    assert_eq!(shown, &opts);
    crate::merge::open(&mut h.app, "merge.panorama").unwrap();
    let Some(crate::state::Dialog::Merge { opts: shown }) = &h.app.ui.dialog else { panic!("no merge dialog") };
    assert_eq!(shown, &crate::merge::MergeDialog::for_command("merge.panorama"));
    h.app.ui.dialog = None;
    // older or hand-edited preferences: missing fields take their defaults, junk is ignored
    h.app.session.last_merge.insert("merge.panorama".into(), json!({"projection": "cylindrical", "command": "merge.hdr"}));
    h.app.session.last_merge.insert("merge.hdrPanorama".into(), json!("junk"));
    let pano = crate::merge::last_options(&h.app, "merge.panorama");
    assert_eq!((pano.command.as_str(), pano.projection.as_str(), pano.auto_crop), ("merge.panorama", "cylindrical", true));
    assert_eq!(crate::merge::last_options(&h.app, "merge.hdrPanorama"), crate::merge::MergeDialog::for_command("merge.hdrPanorama"));
    let _ = std::fs::remove_dir_all(dir);
}

/// HDR for Each Bracket Set runs the merges one after the other, each with its activity row; cancelling one stops the
/// rest.
#[test]
fn batch_merge_runs_each_set_in_turn() {
    let dir = std::env::temp_dir().join(format!("lc-activity-batch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    // one `bracket_dngs` call renders the procedural scene once for all nine inputs (three calls
    // would render it three times); the exposures stay different, or the import would take one
    // set for duplicates of another
    let evs = [-2.0, 0.0, 2.0, -1.5, 0.5, 2.5, -1.0, 1.0, 3.0];
    let sets = ["A", "A", "A", "B", "B", "B", "C", "C", "C"];
    for (i, (set, bytes)) in sets.into_iter().zip(lightcraft_merge::synth::bracket_dngs(1600, 1200, &evs).unwrap()).enumerate() {
        let p = dir.join(format!("{set}_{}.dng", i % 3));
        std::fs::write(&p, bytes).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut session = lightcraft_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(ids.len(), 9);
    for set in ids.chunks(3) {
        session.execute("stack.group", &json!({"ids": set})).unwrap();
    }
    session.execute("library.select", &json!({"ids": ids})).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // the merge ordering does not need the grid's nine thumbnails; one Detail render is enough
    let r = h.request("ui.set", json!({"view": "detail", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(T), "the imported photos and the active preview settle");
    let r = crate::menus::run_ui_command(&mut h.app, "merge.hdrBatch", &json!({})).unwrap().unwrap();
    assert_eq!(r["sets"], 3);
    assert_eq!(h.app.merge.batch.len(), 2, "one running, two queued");
    let rows = h.app.session.activity.list();
    assert_eq!((rows.len(), rows.first().map(|r| r.kind)), (1, Some("merge")));
    // the first merge finishes and the second starts by itself
    assert!(h.step_until(T, |h| h.app.merge.batch.len() == 1 && h.app.merge.final_task.is_some()));
    assert_eq!(h.app.session.catalog.len(), 10);
    // cancelling the second drops the third
    let rows = h.app.session.activity.list();
    h.app.session.activity.cancel(rows[0].id).unwrap();
    assert!(h.step_until(T, |h| h.app.merge.final_task.is_none()));
    assert!(h.app.merge.batch.is_empty());
    assert_eq!(h.app.session.catalog.len(), 10, "nothing more merged");
    assert!(h.app.session.activity.list().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn find_missing_shows_an_indeterminate_row() {
    let mut h = demo();
    let dir = std::env::temp_dir().join(format!("lc-activity-findmissing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // run directly, not through a request (which runs frames): the search of an empty folder is over at once, but its
    // result is applied between frames, and the row is there until then
    let r = crate::menus::run_ui_command(&mut h.app, "file.findMissing", &json!({"folder": dir.to_string_lossy()})).unwrap().unwrap();
    assert_eq!(r["background"], true, "{r}");
    let rows = h.app.session.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str()), ("findMissing", "Find Missing Photos"));
    assert_eq!((rows[0].total, rows[0].cancellable), (0, false), "indeterminate, no ✕");
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()));
    assert!(h.app.session.activity.list().is_empty(), "the row goes with the task");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn auto_import_listing_has_no_row() {
    let mut h = demo();
    let dir = std::env::temp_dir().join(format!("lc-activity-autoimport-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    h.app.session.import_defaults.auto_folder = Some(dir.to_string_lossy().to_string());
    // the watched folder is listed every few seconds, quietly
    assert!(h.step_until(Duration::from_secs(10), |h| h.app.tasks.is_running("Auto Import")));
    assert!(h.app.session.activity.list().is_empty(), "{:?}", h.app.session.activity.list());
    h.app.session.import_defaults.auto_folder = None;
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rows_added_later_grow_the_stack() {
    let mut h = demo();
    let a = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    a.progress(1, 24);
    a.detail("LC01347.jpg");
    wait_visible(&mut h);
    // two more tasks while the stack already shows one row: it grows to show them, nothing is cut off
    let b = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    let c = h.app.session.activity.start("lightroom", "Reading Lightroom catalog", Cancel::Yes);
    wait_visible(&mut h);
    for _ in 0..5 {
        h.step();
    }
    let stack = h.view.ctx.memory(|m| m.area_rect(egui::Id::unique("activity-stack"))).expect("the stack is drawn");
    for g in [&a, &b, &c] {
        let r = rect(&h, &format!("activity:row:{}", g.id()));
        assert!(stack.contains_rect(r), "row {} at {r:?} is cut off by the stack at {stack:?}", g.id());
    }
}

#[test]
fn immediate_library_commands_and_auto_sync_show_completion() {
    let mut h = demo();
    assert!(h.app.run("library.clearPreviews", json!({})).is_ok());
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("✓ Discard Preview Cache"));

    assert_eq!(h.app.run("develop.autoSync", json!({"on": true})).unwrap()["on"], true);
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Auto Sync: On"));

    assert_eq!(h.app.run("develop.autoSync", json!({"on": false})).unwrap()["on"], false);
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Auto Sync: Off"));

    // This demo has no on-disk library to optimize. A failed command must not
    // report completion (or accidentally keep its preceding success toast).
    h.app.ui.toast = None;
    assert!(h.app.run("library.compact", json!({})).is_err());
    assert!(h.app.ui.toast.is_none());
}

/// A read that does not answer until `open`.
#[derive(Clone, Default)]
pub(crate) struct Gate(std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

impl Gate {
    pub(crate) fn wait(&self) {
        let (open, bell) = &*self.0;
        let mut open = open.lock().unwrap();
        while !*open {
            open = bell.wait(open).unwrap();
        }
    }

    pub(crate) fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

/// A folder of four stand-in JPEGs and one (`IMG_hang.jpg`) whose read never answers, and an app that calls a read
/// stalled after 500 ms (a network drive that dropped its connection).
fn hanging_import(tag: &str) -> (Headless, std::path::PathBuf, Gate) {
    hanging(tag, &["IMG_hang.jpg"])
}

/// [`hanging_import`] with the files that never answer named.
fn hanging(tag: &str, hangs: &[&str]) -> (Headless, std::path::PathBuf, Gate) {
    let dir = std::env::temp_dir().join(format!("lc-activity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["IMG_000.jpg", "IMG_001.jpg", "IMG_002.jpg", "IMG_003.jpg"].iter().chain(hangs) {
        std::fs::write(dir.join(name), format!("not really a jpeg {name}")).unwrap();
    }
    let gate = Gate::default();
    let mut s = lightcraft_engine::Session::new();
    s.import_stall_after = Duration::from_millis(500);
    // (its own count of blocked readers: tests share the process)
    s.stuck_readers = std::sync::Arc::new(lightcraft_engine::import::StuckReaders::new(lightcraft_engine::import::MAX_STUCK_READERS));
    let in_probe = gate.clone();
    s.media.file_probe = Some(std::sync::Arc::new(move |p: &str| {
        if p.contains("IMG_hang") {
            in_probe.wait();
        }
        Ok(lightcraft_engine::media::ProbeInfo {
            width: 60,
            height: 40,
            format: "JPEG".into(),
            content_hash: Some(p.to_string()),
            ..Default::default()
        })
    }));
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    (h, dir, gate)
}

/// The import's row once it says it waits for a file.
fn stalled_row(h: &mut Headless) -> TaskInfo {
    assert!(h.step_until(Duration::from_secs(30), |h| h.app.session.activity.list().first().is_some_and(|t| t.stalled.is_some())), "no stall");
    h.app.session.activity.list().remove(0)
}

/// Given an import whose read does not answer,
/// When ✕ is pressed,
/// Then the import ends within a second or two, "Stopping…" does not last.
#[test]
fn cancel_ends_an_import_whose_read_hangs() {
    let (mut h, dir, gate) = hanging_import("cancel-hang");
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    let row = stalled_row(&mut h);
    h.app.session.activity.cancel(row.id).unwrap();
    let pressed = std::time::Instant::now();
    assert!(h.step_until(Duration::from_secs(10), |h| h.app.import.is_none()), "still stopping");
    assert!(pressed.elapsed() < Duration::from_millis(2500), "took {:?}", pressed.elapsed());
    gate.open();
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Import cancelled")), "{:?}", h.app.ui.toast);
    assert!(h.app.session.activity.list().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `activity.skip` through the control channel: an unknown id is an error, and for the stalled import it skips the
/// file.
#[test]
fn activity_skip_command_skips_the_stalled_file() {
    let (mut h, dir, gate) = hanging_import("skip-command");
    assert_eq!(h.request("engine.execute", json!({"command": "activity.skip", "params": {"id": 999}}), T)["ok"], false);
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    let row = stalled_row(&mut h);
    let listed = h.request("engine.execute", json!({"command": "activity.list", "params": {}}), T);
    assert_eq!(listed["result"]["tasks"][0]["stalled"]["what"], "IMG_hang.jpg", "{listed}");
    let r = h.request("engine.execute", json!({"command": "activity.skip", "params": {"id": row.id}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(Duration::from_secs(30), |h| h.app.import.is_none()));
    gate.open();
    assert_eq!(h.app.session.catalog.len(), 4);
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("1 not readable")), "{:?}", h.app.ui.toast);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The text of the toast on screen, if any.
fn toast_text(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

/// Given four files of an import that never answer and four that do,
/// When each hung file is skipped as the row names it,
/// Then at the fourth the import ends by itself with an error that the drive is not answering, the files readied
/// before it stay, and its row goes.
#[test]
fn an_import_ends_with_an_error_at_the_fourth_skipped_read() {
    let (mut h, dir, gate) = hanging("cap", &["IMG_hang0.jpg", "IMG_hang1.jpg", "IMG_hang2.jpg", "IMG_hang3.jpg"]);
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    let ended = h.step_until(Duration::from_secs(60), |h| {
        if let Some(row) = h.app.session.activity.list().first()
            && row.stalled.is_some()
        {
            let _ = h.app.session.activity.skip(row.id);
        }
        h.app.import.is_none()
    });
    gate.open();
    assert!(ended, "the import never ended");
    let toast = toast_text(&h);
    assert!(toast.contains("Import failed:") && toast.contains("not answering") && toast.contains("IMG_hang"), "{toast}");
    assert!(h.app.session.activity.list().is_empty(), "the row goes");
    assert!(h.app.session.catalog.len() <= 4, "no more than the files that answered");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the import review's scan of a folder with a file that never answers,
/// When its row names the file and Skip is asked for (`activity.skip`),
/// Then the review opens and lists that file as unreadable, with the skip as its reason.
#[test]
fn skip_on_the_review_scans_row_lists_the_file_as_unreadable() {
    let (mut h, dir, gate) = hanging_import("scan-skip");
    crate::import::open(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    let row = stalled_row(&mut h);
    assert_eq!(row.kind, "scan", "{row:?}");
    assert_eq!(row.stalled.as_ref().map(|s| s.what.as_str()), Some("IMG_hang.jpg"));
    let r = h.request("engine.execute", json!({"command": "activity.skip", "params": {"id": row.id}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(Duration::from_secs(30), |h| h.app.scan.is_none()), "the scan goes on");
    gate.open();
    let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no review: {:?}", h.app.ui.dialog.is_some()) };
    let hung = opts.candidates.iter().find(|c| c.name == "IMG_hang.jpg").expect("the file is listed");
    assert!(hung.error.as_deref().is_some_and(|e| e.starts_with("skipped: no answer after ")), "{hung:?}");
    assert_eq!(opts.candidates.iter().filter(|c| c.error.is_none()).count(), 4, "the others were read");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same for the Local view's reading of a folder (the browse): the row names the file and offers Skip.
#[test]
fn skip_on_the_browse_scans_row_goes_on_with_the_other_files() {
    let (mut h, dir, gate) = hanging_import("browse-skip");
    crate::import::browse(&mut h.app, &dir.to_string_lossy(), None).unwrap();
    let row = stalled_row(&mut h);
    assert_eq!(row.kind, "scan", "{row:?}");
    let r = h.request("engine.execute", json!({"command": "activity.skip", "params": {"id": row.id}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(Duration::from_secs(30), |h| h.app.scan.is_none() && h.app.import.is_none()));
    gate.open();
    assert!(h.app.session.catalog.len() >= 4, "the files that answered are in Local: {}", h.app.session.catalog.len());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the Local view's reading of a folder whose files stop answering one after another,
/// When the fourth is skipped,
/// Then an error toast says the folder was read only as far as the drive answered.
#[test]
fn a_browse_that_stops_at_the_cap_says_so() {
    let (mut h, dir, gate) = hanging("browse-cap", &["IMG_hang0.jpg", "IMG_hang1.jpg", "IMG_hang2.jpg", "IMG_hang3.jpg"]);
    crate::import::browse(&mut h.app, &dir.to_string_lossy(), None).unwrap();
    let ended = h.step_until(Duration::from_secs(60), |h| {
        if let Some(row) = h.app.session.activity.list().first()
            && row.stalled.is_some()
        {
            let _ = h.app.session.activity.skip(row.id);
        }
        h.app.scan.is_none() && h.app.ui.toast.is_some()
    });
    gate.open();
    assert!(ended, "no toast: {:?}", toast_text(&h));
    let toast = toast_text(&h);
    assert!(toast.starts_with("Stopped:") && toast.contains("not answering"), "{toast}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the import review's scan of a folder whose files stop answering one after another, and another dialog open
/// when the scan ends (so the review waits, and a toast made then would be gone by the time it opens),
/// When that dialog is closed and the review opens,
/// Then an error toast says the folder was read only as far as the drive answered.
#[test]
fn a_review_that_waits_for_a_dialog_says_the_scan_stopped_when_it_opens() {
    let (mut h, dir, gate) = hanging("review-cap", &["IMG_hang0.jpg", "IMG_hang1.jpg", "IMG_hang2.jpg", "IMG_hang3.jpg"]);
    crate::import::open(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    h.app.ui.dialog = Some(crate::state::Dialog::WhatsNew);
    let ended = h.step_until(Duration::from_secs(60), |h| {
        if let Some(row) = h.app.session.activity.list().first()
            && row.stalled.is_some()
        {
            let _ = h.app.session.activity.skip(row.id);
        }
        h.app.scan.is_none()
    });
    gate.open();
    assert!(ended, "the scan never ended");
    // (a toast made at the scan's end, if there was one, is over by now)
    h.app.ui.toast = None;
    h.step();
    assert!(h.app.review_pending.is_some(), "the review waits for the open dialog");
    h.app.ui.dialog = None;
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })), "the review opened");
    let toast = toast_text(&h);
    assert!(toast.starts_with("Stopped:") && toast.contains("not answering"), "{toast}");
    let _ = std::fs::remove_dir_all(&dir);
}
