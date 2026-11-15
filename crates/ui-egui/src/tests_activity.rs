//! Issue #345: the activity stack — one place for every long-running task, with ✕ to cancel. In the Classic layout
//! the tasks take the identity plate's place in the module bar, and a click there lists them all; Modern keeps its card.

use std::time::Duration;

use lcc_engine::activity::{Cancel, Unit};
use serde_json::json;

use crate::headless::Headless;
use crate::panels::activity::{LIST, PLATE, WIDTH};
use crate::{LccApp, Services};

// generous: merges and renders run on worker threads that a busy machine (a full workspace test run) slows a lot
const T: Duration = Duration::from_secs(60);

fn demo() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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

fn bars(h: &Headless) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with("activity:bar:")).count()
}

/// Open the Classic list by clicking the tasks in the module bar.
fn open_list(h: &mut Headless) {
    click(h, PLATE);
    assert!(h.app.activity_list, "the list opened");
    h.step();
    assert!(has(h, LIST), "the list is drawn");
}

/// The demo library in the Modern layout, whose activity is a floating card.
fn modern() -> Headless {
    let mut h = demo();
    let r = h.request("engine.execute", json!({"command": "view.modernLayout", "params": {"on": true}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    h
}

/// One Classic session: the plate and the status line get their own clicks; a task takes their place and gives it
/// back; the header's ✕ cancels; a task that can't be cancelled has none.
#[test]
fn a_task_takes_the_identity_plates_place_and_gives_it_back() {
    let mut h = demo();
    // the module bar's own click area sits under the plate and the status line: a right-click on the plate opens
    // the plate's menu (not the module picker's), and a click on the status line dismisses it
    let bold = h.app.ui.identity_plate.bold;
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "region:identityPlate", "button": "right"}), T)["ok"], true);
    for _ in 0..3 {
        h.step();
    }
    assert!(has(&h, "plate:bold"), "the plate's menu opened");
    click(&mut h, "plate:bold");
    assert_eq!(h.app.ui.identity_plate.bold, !bold, "Bold in the plate's menu");
    h.app.ui.status = "Ready".into();
    h.step();
    h.step();
    click(&mut h, "status:message");
    assert!(h.app.ui.status.is_empty(), "a click dismissed the message");

    // a task takes the plate's and the status line's place, half a second in
    h.app.ui.status = "Ready".into();
    h.step();
    h.step();
    assert!(has(&h, "region:identityPlate") && has(&h, "status:message"), "the plate and the status while nothing runs");
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    g.progress(2, 5);
    h.step();
    // a frame on a loaded machine can outlast the half second: only judge a task still young after it
    if h.app.session.activity.list()[0].age_ms < crate::panels::activity::SHOW_AFTER_MS {
        assert!(!has(&h, PLATE), "not before 0.5 s");
        assert!(has(&h, "region:identityPlate"), "the plate stays until then");
    }
    wait_visible(&mut h);
    assert!(has(&h, PLATE), "the task shows in the module bar");
    assert!(!has(&h, "region:identityPlate"), "in the plate's place");
    assert!(!has(&h, "status:message"), "and in the status line's");
    let bar = rect(&h, "region:moduleBar");
    assert!(bar.contains_rect(rect(&h, PLATE)), "inside the module bar: {:?} in {bar:?}", rect(&h, PLATE));
    assert!(has(&h, &format!("activity:cancel:{}", g.id())), "one task has its ✕ in the header");
    let card = h.view.ctx.memory(|m| m.area_rect(egui::Id::new("activity-stack")));
    assert!(card.is_none() && rows(&h) == 0, "no floating card in the Classic layout: {card:?}");
    drop(g);
    h.step();
    h.step();
    assert!(!has(&h, PLATE), "gone with the task");
    assert!(has(&h, "region:identityPlate") && has(&h, "status:message"), "the plate and the status line come back");

    // the ✕ in the header cancels, and doesn't open the list
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    click(&mut h, &format!("activity:cancel:{}", g.id()));
    assert!(g.is_cancelled(), "the header's ✕ cancels");
    assert!(!h.app.activity_list, "✕ cancels; it doesn't open the list");
    let inspect = h.request("ui.inspect", json!({}), T);
    assert_eq!(inspect["result"]["activity"][0]["cancelling"], true, "{}", inspect["result"]["activity"]);
    drop(g);
    h.step();
    h.step();

    // a task that can't be cancelled has no ✕, in the header or the list
    let g = h.app.session.activity.start("faces", "Finding faces", Cancel::No);
    wait_visible(&mut h);
    assert!(has(&h, PLATE), "the uncancellable task shows");
    assert!(!has(&h, &format!("activity:cancel:{}", g.id())), "no ✕ in the header");
    open_list(&mut h);
    assert!(has(&h, &format!("activity:row:{}", g.id())), "its row is listed");
    assert!(!has(&h, &format!("activity:cancel:{}", g.id())), "no ✕ in the list");
}

/// Several tasks: a thin bar each, oldest first, with no ✕; a batch's photos add none; past what fits in the
/// module bar, the oldest show.
#[test]
fn several_tasks_show_a_thin_bar_each_inside_the_module_bar() {
    let mut h = demo();
    let a = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    let b = h.app.session.activity.start_batch("denoise", "AI Denoise", Cancel::Yes);
    let _photo = b.child("denoise", "AI Denoise");
    wait_visible(&mut h);
    assert_eq!(bars(&h), 2, "one bar per task, the batch's photo adds none");
    for g in [&a, &b] {
        assert!(!has(&h, &format!("activity:cancel:{}", g.id())), "no ✕ while several run");
    }
    assert!(rect(&h, &format!("activity:bar:{}", a.id())).top() < rect(&h, &format!("activity:bar:{}", b.id())).top(), "oldest first");
    let more: Vec<_> = (0..8).map(|i| h.app.session.activity.start("export", &format!("Task {i}"), Cancel::Yes)).collect();
    wait_visible(&mut h);
    let module_bar = rect(&h, "region:moduleBar");
    let shown: Vec<egui::Rect> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("activity:bar:")).map(|(_, r)| *r).collect();
    assert!(!shown.is_empty() && shown.len() < 10, "{} of 10 bars fit", shown.len());
    for r in &shown {
        assert!(module_bar.contains_rect(*r), "bar {r:?} spills out of the module bar {module_bar:?}");
    }
    assert!(has(&h, &format!("activity:bar:{}", a.id())), "the oldest show");
    assert!(!has(&h, &format!("activity:bar:{}", more[7].id())), "the newest wait");
}

/// A click on the tasks lists every one under the module bar, each with its row and ✕; Esc, a click outside or the
/// last task ending closes it.
#[test]
fn the_list_shows_every_task_and_closes() {
    let mut h = demo();
    let guards: Vec<_> = (0..5).map(|i| h.app.session.activity.start("export", &format!("Task {i}"), Cancel::Yes)).collect();
    wait_visible(&mut h);
    assert_eq!(rows(&h), 0, "closed at first");
    open_list(&mut h);
    let inspect = h.request("ui.inspect", json!({}), T);
    assert_eq!(inspect["result"]["activityList"], true, "{}", inspect["result"]["activityList"]);
    let list = rect(&h, LIST);
    assert!(list.top() >= rect(&h, "region:moduleBar").bottom() - 1.0, "under the module bar: {list:?}");
    for g in &guards {
        let r = rect(&h, &format!("activity:row:{}", g.id()));
        assert!(list.contains_rect(r), "row {} at {r:?} is cut off by the list at {list:?}", g.id());
        assert!(has(&h, &format!("activity:cancel:{}", g.id())), "each has its ✕");
    }
    // ✕ in the list cancels that task, and the list stays
    click(&mut h, &format!("activity:cancel:{}", guards[1].id()));
    assert!(guards[1].is_cancelled() && !guards[0].is_cancelled());
    assert!(h.app.activity_list, "still open");
    // Esc closes it, and only it: the loupe stays (Esc alone would go back to the grid)
    let first = h.app.session.visible_cloned()[0].0;
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first]}}), T);
    assert_eq!(h.request("engine.execute", json!({"command": "view.loupe", "params": {}}), T)["ok"], true);
    h.step();
    let loupe = h.app.ui.view;
    assert!(h.app.activity_list, "still open in the loupe");
    h.request("ui.key", json!({"key": "Escape"}), T);
    for _ in 0..3 {
        h.step();
    }
    assert!(!h.app.activity_list && !has(&h, LIST), "Esc closed it");
    assert_eq!(h.app.ui.view, loupe, "Esc did nothing else");
    // a click outside closes it
    open_list(&mut h);
    h.request("ui.click", json!({"x": 600.0, "y": 500.0}), T);
    for _ in 0..3 {
        h.step();
    }
    assert!(!h.app.activity_list, "a click outside closed it");
    // the last task ending closes it
    open_list(&mut h);
    drop(guards);
    h.step();
    h.step();
    assert!(!h.app.activity_list && !has(&h, LIST));
}

#[test]
fn the_list_opens_and_closes_by_command() {
    let mut h = demo();
    let exec = |h: &mut Headless, p: serde_json::Value| h.request("engine.execute", json!({"command": "view.activityList", "params": p}), T);
    let r = exec(&mut h, json!({"open": true}));
    assert_eq!(r["ok"], false, "nothing to list yet: {r}");
    assert!(!h.app.activity_list);
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    let r = exec(&mut h, json!({"open": true}));
    assert_eq!((r["ok"].clone(), r["result"]["open"].clone()), (json!(true), json!(true)), "{r}");
    h.step();
    assert!(has(&h, &format!("activity:row:{}", g.id())));
    // without a parameter it toggles
    let r = exec(&mut h, json!({}));
    assert_eq!(r["result"]["open"], false, "{r}");
    h.step();
    assert!(!has(&h, LIST));
    assert_eq!(h.request("ui.inspect", json!({}), T)["result"]["activityList"], false);
}

/// With the module bar hidden or Show Status and Activity off, no activity shows; the tasks run on, are listed, and
/// Quit still asks.
#[test]
fn hidden_module_bar_or_status_off_shows_nothing_and_tasks_run_on() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    assert!(has(&h, PLATE));
    open_list(&mut h);
    let hide = h.request("engine.execute", json!({"command": "panel.top", "params": {"show": false}}), T);
    assert_eq!(hide["ok"], true, "{hide}");
    h.step();
    h.step();
    assert!(!has(&h, PLATE) && !has(&h, LIST) && rows(&h) == 0, "nothing shows with the module bar hidden");
    assert!(!h.app.activity_list, "the list closed with it");
    assert!(!g.is_cancelled());
    assert_eq!(h.request("engine.execute", json!({"command": "activity.list", "params": {}}), T)["result"]["tasks"][0]["id"], g.id());
    h.request("engine.execute", json!({"command": "panel.top", "params": {"show": true}}), T);
    let off = h.request("engine.execute", json!({"command": "view.identityPlate", "params": {"showStatus": false}}), T);
    assert_eq!(off["ok"], true, "{off}");
    h.step();
    h.step();
    assert!(!has(&h, PLATE) && rows(&h) == 0, "nothing shows with Show Status and Activity off");
    assert!(has(&h, "region:identityPlate"), "the plate stays");
    assert_eq!(h.request("engine.execute", json!({"command": "view.activityList", "params": {"open": true}}), T)["ok"], false);
    assert!(!g.is_cancelled() && h.app.session.activity.list().len() == 1, "still running");
    assert!(!crate::panels::notices::may_close(&mut h.app), "Quit still asks");
}

#[test]
fn modern_keeps_its_card_and_overflows() {
    let mut h = modern();
    let guards: Vec<_> = (0..5).map(|i| h.app.session.activity.start("export", &format!("Task {i}"), Cancel::Yes)).collect();
    wait_visible(&mut h);
    assert!(!has(&h, PLATE), "no module bar activity in Modern");
    assert_eq!(rows(&h), 3);
    assert!(has(&h, "activity:more"));
    click(&mut h, "activity:more");
    assert_eq!(rows(&h), 5);
    // expanded, the stack grows to show them all
    let stack = h.view.ctx.memory(|m| m.area_rect(egui::Id::new("activity-stack"))).expect("the stack is drawn");
    for g in &guards {
        let r = rect(&h, &format!("activity:row:{}", g.id()));
        assert!(stack.contains_rect(r), "row {} at {r:?} is cut off by the stack at {stack:?}", g.id());
    }
    drop(guards);
    h.step();
    assert_eq!(rows(&h), 0);
    // the card asks before it stops a plug-in's wait
    let (g, flag) = wait(&h);
    wait_visible(&mut h);
    assert!(has(&h, &format!("activity:row:{}", g.id())), "the wait has its row in the card");
    assert!(!has(&h, &format!("activity:cancel:{}", g.id())), "no ✕ in the card");
    stop_waiting(&mut h, g.id(), "button:dialogCancel");
    assert!(!stopped(&flag), "Cancel leaves the run waiting");
    stop_waiting(&mut h, g.id(), "button:dialogOk");
    assert!(stopped(&flag), "confirming stops the wait");
}

/// The demo library in the Compact layout: Classic drawn denser, with the same module bar.
fn compact() -> Headless {
    let mut h = demo();
    set_layout(&mut h, "view.compactLayout");
    h
}

fn set_layout(h: &mut Headless, command: &str) {
    let r = h.request("engine.execute", json!({"command": command, "params": {"on": true}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

/// Issue #847: Compact turns Modern off, so its tasks show as Classic's do: in the identity plate's place, with the
/// list under them and no floating card.
#[test]
fn compact_shows_tasks_in_the_module_bar_and_lists_them_as_classic() {
    let mut h = compact();
    assert!(h.app.compact() && crate::theme::compact(&h.view.ctx), "the Compact layout is on");
    let guards: Vec<_> = (0..3).map(|i| h.app.session.activity.start("export", &format!("Task {i}"), Cancel::Yes)).collect();
    wait_visible(&mut h);
    assert!(has(&h, PLATE) && !has(&h, "region:identityPlate"), "the tasks take the plate's place");
    let bar = rect(&h, "region:moduleBar");
    assert!(bar.contains_rect(rect(&h, PLATE)), "inside the module bar: {:?} in {bar:?}", rect(&h, PLATE));
    let card = h.view.ctx.memory(|m| m.area_rect(egui::Id::new("activity-stack")));
    assert!(card.is_none() && rows(&h) == 0, "no floating card in Compact: {card:?}");
    // a click opens the list under the tasks, every row inside it
    open_list(&mut h);
    let list = rect(&h, LIST);
    assert!(list.top() >= bar.bottom() - 1.0, "under the module bar: {list:?}");
    for g in &guards {
        let r = rect(&h, &format!("activity:row:{}", g.id()));
        assert!(list.contains_rect(r), "row {} at {r:?} is cut off by the list at {list:?}", g.id());
    }
    h.request("ui.key", json!({"key": "Escape"}), T);
    for _ in 0..3 {
        h.step();
    }
    assert!(!h.app.activity_list && !has(&h, LIST), "Esc closed it");
    // and by command
    let r = h.request("engine.execute", json!({"command": "view.activityList", "params": {"open": true}}), T);
    assert_eq!(r["result"]["open"], true, "{r}");
    h.step();
    assert!(has(&h, LIST) && rows(&h) == guards.len(), "the command opens it with every row");
}

/// Issue #847: Classic and Compact share the module bar, so the list stays open across a switch between them, under
/// the tasks; Modern closes it, from either.
#[test]
fn switching_between_classic_and_compact_keeps_the_list_open() {
    let mut h = demo();
    let _g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    open_list(&mut h);
    for (command, on) in [("view.compactLayout", true), ("view.compactLayout", false), ("view.compactLayout", true)] {
        let r = h.request("engine.execute", json!({"command": command, "params": {"on": on}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!(h.app.compact(), on);
        assert!(h.app.activity_list && has(&h, LIST), "the list stays open (compact: {on})");
        let (list, tasks) = (rect(&h, LIST), rect(&h, PLATE));
        assert!(list.top() >= tasks.bottom() && (list.left() - tasks.left()).abs() < 1.0, "under the tasks: {list:?} vs {tasks:?}");
    }
    set_layout(&mut h, "view.modernLayout");
    assert!(!h.app.activity_list, "Modern closes it, from Compact too");
}

/// a batch row in the list folds its photos, with the time left and the failures; when it ends, what it did shows
/// once as a message.
#[test]
fn a_batch_row_folds_its_photos_and_says_how_it_went() {
    use crate::panels::activity::{eta_text, summary_text};
    assert_eq!(eta_text(20_000), "less than a minute left");
    assert_eq!(eta_text(6 * 60_000), "about 6 min left");
    assert_eq!(eta_text(150 * 60_000), "about 2 h 30 min left");
    let mut h = demo();
    let b = h.app.session.activity.start_batch("denoise", "AI Denoise", Cancel::Yes);
    b.progress(0, 200);
    std::thread::sleep(Duration::from_millis(30));
    b.item_done();
    b.item_failed("IMG_0002.CR3: the file is damaged");
    let photo = b.child("denoise", "AI Denoise");
    photo.detail("IMG_0003.CR3");
    wait_visible(&mut h);
    open_list(&mut h);
    let parent = format!("activity:row:{}", b.id());
    let child = format!("activity:row:{}", photo.id());
    assert!(has(&h, &parent));
    assert!(!has(&h, &child), "the photo's row is folded under its batch");
    let line = crate::panels::activity::detail_line_for_tests(&h.app.session.activity.list()[0]);
    assert!(line.starts_with("2 of 200 · about ") && line.contains("1 failed"), "{line}");
    let fold = format!("activity:children:{}", b.id());
    click(&mut h, &fold);
    assert!(has(&h, &child), "opened");
    assert!(h.app.activity_list, "a click inside the list keeps it open");
    assert!(rect(&h, &child).left() > rect(&h, &parent).left(), "set in under its batch");
    click(&mut h, &fold);
    assert!(!has(&h, &child), "folded again");
    // the batch ends: its summary shows once
    drop(photo);
    drop(b);
    h.step();
    let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
    assert_eq!(toast, "AI Denoise: 1 done, 1 failed: IMG_0002.CR3: the file is damaged");
    h.app.ui.toasts.clear();
    h.step();
    assert!(h.app.ui.toast().is_none(), "said once");
    // a job that reports itself (an export) isn't repeated
    let e = h.app.session.activity.start_batch("export", "Exporting", Cancel::Yes);
    e.reports_itself();
    e.item_done();
    drop(e);
    h.step();
    assert!(h.app.ui.toast().is_none());
    let s = h.app.session.activity.finished();
    assert_eq!(summary_text(s.last().unwrap()), "Exporting: 1 done");
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
    assert!(h.app.ui.toast().is_none(), "{:?}", h.app.ui.toasts);
    // the same through the row's ✕ (clicked in the frame the task ended): no message in the status bar either
    h.app.ui.status = "Ready".into();
    crate::panels::activity::cancel(&mut h.app, id);
    h.step();
    assert_eq!(h.app.ui.status, "Ready");
    assert!(h.app.ui.toast().is_none(), "{:?}", h.app.ui.toasts);
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
    if let Some(crate::state::Dialog::Export { opts, full_size, resize, dir, .. }) = &mut h.app.ui.dialog {
        opts.destination_mode = lcc_engine::export::DestinationMode::SpecificFolder;
        *full_size = false;
        *resize = lcc_engine::export::Resize::long_edge(64);
        *dir = "/lc-test-out".into();
    }
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
}

/// Running tasks hold Quit back until the user answers; a task already cancelling, or one that can't be cancelled,
/// doesn't.
#[test]
fn quit_with_a_running_task_asks_and_quit_anyway_cancels() {
    let mut h = demo();
    {
        let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
        h.app.session.activity.cancel(g.id()).unwrap();
        let _f = h.app.session.activity.start("faces", "Finding faces", Cancel::No);
        assert!(crate::panels::notices::may_close(&mut h.app), "a cancelling task doesn't block quit");
    }
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    assert!(!crate::panels::notices::may_close(&mut h.app));
    assert!(matches!(h.app.quit_prompt, Some(crate::QuitPrompt::Tasks(_))), "{:?}", h.app.quit_prompt);
    h.step();
    assert!(!has(&h, "button:quitRetry"));
    let text = h.painted_text().join("\n");
    assert!(!text.contains("Disable backups"), "only the backup prompt points at the backup setting:\n{text}");
    click(&mut h, "button:quitCancel");
    assert!(!g.is_cancelled() && h.app.quit_prompt.is_none() && !h.app.quit_confirmed, "Cancel keeps the task running");
    assert!(!crate::panels::notices::may_close(&mut h.app), "the next quit asks again");
    click(&mut h, "button:quitAnyway");
    assert!(g.is_cancelled() && h.quit_requested());
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
    // a worker that dies leaves no row behind
    h.app.services.write_shared = Some(std::sync::Arc::new(|_: &str, _: &[u8]| -> Result<(), String> { panic!("synthetic writer panic") }));
    start_export(&mut h, 2);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()), "the dead export ends");
    assert!(h.app.session.activity.list().is_empty(), "a dead worker leaves no row");
}

/// A folder of `n` stand-in JPEGs and an app whose file probe takes 100 ms each (a slow drive).
fn slow_import(tag: &str, n: usize) -> (lcc_testkit::TempDir, Headless) {
    gated_import(tag, n, None)
}

/// Like `slow_import`, and while `gate` is false the probe holds every file after the first batch
/// (`batch_size()` files, which join the catalog together), so a test can act mid-import without
/// racing the 100 ms-per-file clock. The hold gives up after 60 s, so a failed test never leaves
/// the import thread spinning.
fn gated_import(tag: &str, n: usize, gate: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> (lcc_testkit::TempDir, Headless) {
    let dir = lcc_testkit::TempDir::new(&format!("lc-activity-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..n {
        std::fs::write(dir.join(format!("IMG_{i:03}.jpg")), format!("not really a jpeg {i}")).unwrap();
    }
    let mut s = lcc_engine::Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(move |p: &str| {
        std::thread::sleep(Duration::from_millis(100));
        let index = p.rsplit_once("IMG_").and_then(|(_, n)| n.strip_suffix(".jpg")).and_then(|n| n.parse::<usize>().ok());
        if let (Some(gate), Some(index)) = (&gate, index)
            && index >= lcc_engine::import::batch_size()
        {
            let t0 = std::time::Instant::now();
            while !gate.load(std::sync::atomic::Ordering::Acquire) && t0.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(lcc_engine::media::ProbeInfo { width: 60, height: 40, format: "JPEG".into(), content_hash: Some(p.to_string()), ..Default::default() })
    }));
    let mut h = Headless::new(LccApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    (dir, h)
}

#[test]
fn import_cancelled_by_command_reports_cancelled() {
    let n = 40;
    let (dir, mut h) = slow_import("cancel", n);
    let undo0 = h.app.session.undo.len();
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.as_ref().is_some_and(|t| t.imported >= 2)));
    let tasks = h.app.session.activity.list();
    assert_eq!(tasks.first().map(|t| t.kind), Some("import"), "{tasks:?}");
    h.app.session.activity.cancel(tasks[0].id).unwrap();
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.is_none()));
    assert!(h.app.session.activity.list().is_empty());
    assert!(h.app.ui.toast().is_some_and(|t| t.0.starts_with("Import cancelled")), "{:?}", h.app.ui.toasts);
    assert!(h.app.session.catalog.len() < n, "stopped part-way");
    assert_eq!(h.app.session.undo.len(), undo0 + 1, "one undo step");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A photo chosen while an import runs stays the active one when the import ends: the import
/// selects its first photo only if nobody picked something else meanwhile (issue #1446).
#[test]
fn a_photo_chosen_during_an_import_stays_selected() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (dir, mut h) = gated_import("chosen", 20, Some(gate.clone()));
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
    let (dir, mut h) = slow_import("scan", 120);
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
fn bracket(tag: &str) -> (lcc_testkit::TempDir, Headless) {
    let dir = lcc_testkit::TempDir::new(&format!("lc-activity-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for (i, bytes) in lcc_merge::synth::bracket_dngs(1600, 1200, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, bytes).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut session = lcc_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    session.execute("library.select", &json!({"ids": ids})).unwrap();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    (dir, h)
}

#[test]
fn final_merge_shows_a_row_and_cancels() {
    for (command, cancelled) in [("merge.hdr", "HDR merge cancelled"), ("merge.focusStack", "Focus stack merge cancelled")] {
        let (dir, mut h) = bracket(command);
        let before = h.app.session.catalog.len();
        crate::merge::start_last(&mut h.app, command).unwrap();
        h.step();
        let rows = h.app.session.activity.list();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].unit), ("merge", "Merging", Unit::Percent));
        assert!(rows[0].cancellable);
        assert_eq!(rows[0].total, 1000);
        h.app.session.activity.cancel(rows[0].id).unwrap();
        assert!(h.step_until(T, |h| h.app.merge.final_task.is_none()));
        assert!(h.app.merge.last_result.is_none(), "{command}: nothing merged");
        assert_eq!(h.app.session.catalog.len(), before, "{command}: no photo added");
        assert!(h.app.session.activity.list().is_empty());
        let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
        assert_eq!(toast, cancelled);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The merge dialog opens with the options of the last merge of its kind (per kind), and "with Last
/// Settings" uses them; a kind never merged opens with the defaults.
#[test]
fn merge_dialog_remembers_the_last_settings_per_kind() {
    let (dir, mut h) = bracket("merge-last");
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
    let dir = lcc_testkit::TempDir::new("lc-activity-batch");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    // different exposures, or the import would take set B for duplicates of set A
    for (set, evs) in [("A", [-2.0, 0.0, 2.0]), ("B", [-1.5, 0.5, 2.5]), ("C", [-1.0, 1.0, 3.0])] {
        for (i, bytes) in lcc_merge::synth::bracket_dngs(1600, 1200, &evs).unwrap().into_iter().enumerate() {
            let p = dir.join(format!("{set}_{i}.dng"));
            std::fs::write(&p, bytes).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
    }
    let mut session = lcc_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(ids.len(), 9);
    for set in ids.chunks(3) {
        session.execute("stack.group", &json!({"ids": set})).unwrap();
    }
    session.execute("library.select", &json!({"ids": ids})).unwrap();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
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
    let dir = lcc_testkit::TempDir::new("lc-activity-findmissing");
    let mut h = demo();
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
fn validate_dng_files_runs_in_the_background_and_lists_what_it_found() {
    // nothing to check: a refusal, not an empty dialog
    let mut h = demo();
    let err = crate::menus::run_ui_command(&mut h.app, "library.validateDng", &json!({})).unwrap().unwrap_err();
    assert!(err.contains("no DNG files"), "{err}");

    let (dir, mut h) = bracket("validate-dng");
    let r = crate::menus::run_ui_command(&mut h.app, "library.validateDng", &json!({})).unwrap().unwrap();
    assert_eq!((r["background"].as_bool(), r["total"].as_u64()), (Some(true), Some(3)), "{r}");
    let rows = h.app.session.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].cancellable), ("validateDng", "Validate DNG Files", true));
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()));
    assert!(h.app.session.activity.list().is_empty(), "the row goes with the task");
    // the answer is a dialog with the counts, and a report for scripts
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::DngValidation { summary, .. }) if summary.contains('3')), "{:?}", h.app.ui.dialog);
    let report = h.app.ui.last_dng_validation.clone().unwrap();
    assert_eq!((report["total"].as_u64(), report["checked"].as_u64(), report["cancelled"].as_bool()), (Some(3), Some(3), Some(false)), "{report}");
    let counted = report["intact"].as_u64().unwrap()
        + report["damaged"].as_array().unwrap().len() as u64
        + report["unverifiable"].as_array().unwrap().len() as u64;
    assert_eq!(counted, 3, "every file gets a verdict: {report}");

    // the same through `wait`, with one file now cut short: damaged, whatever the others are
    h.app.ui.dialog = None;
    let first = h.app.session.catalog.photos().next().unwrap().id;
    let path = match &h.app.session.catalog.photo(first).unwrap().source {
        lcc_catalog::Source::File { path } => path.clone(),
        other => panic!("{other:?}"),
    };
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 3]).unwrap();
    let r = crate::menus::run_ui_command(&mut h.app, "library.validateDng", &json!({"wait": true})).unwrap().unwrap();
    assert!(r["damaged"].as_array().unwrap().iter().any(|d| d["file"].as_str().is_some_and(|f| path.ends_with(f))), "{r}");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::DngValidation { damaged, .. }) if damaged.len() == 1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn auto_import_listing_has_no_row() {
    let dir = lcc_testkit::TempDir::new("lc-activity-autoimport");
    let mut h = demo();
    std::fs::create_dir_all(&dir).unwrap();
    h.app.session.import_defaults.auto_folder = Some(dir.to_string_lossy().to_string());
    // the watched folder is listed every few seconds, quietly
    assert!(h.step_until(Duration::from_secs(10), |h| h.app.tasks.is_running("Auto Import")));
    assert!(h.app.session.activity.list().is_empty(), "{:?}", h.app.session.activity.list());
    h.app.session.import_defaults.auto_folder = None;
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rows_added_later_grow_the_list_and_long_detail_stays_inside() {
    let mut h = demo();
    let a = h.app.session.activity.start("export", "Exportieren", Cancel::Yes);
    a.progress(1, 24);
    a.detail(&"IMG_".repeat(75));
    wait_visible(&mut h);
    assert!(rect(&h, PLATE).width() <= WIDTH + 1.0, "a long detail stays inside the header: {:?}", rect(&h, PLATE));
    open_list(&mut h);
    let row = rect(&h, &format!("activity:row:{}", a.id()));
    assert!(row.width() <= WIDTH + 1.0, "a long detail stays inside the list: {row:?}");
    // two more tasks while the list already shows one row: it grows to show them, nothing is cut off
    let b = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    let c = h.app.session.activity.start("lightroom", "Reading Lightroom catalog", Cancel::Yes);
    wait_visible(&mut h);
    for _ in 0..5 {
        h.step();
    }
    let list = rect(&h, LIST);
    for g in [&a, &b, &c] {
        let r = rect(&h, &format!("activity:row:{}", g.id()));
        assert!(list.contains_rect(r), "row {} at {r:?} is cut off by the list at {list:?}", g.id());
    }
}

#[test]
fn immediate_library_commands_and_auto_sync_show_completion() {
    let mut h = demo();
    assert!(h.app.run("library.clearPreviews", json!({})).is_ok());
    assert_eq!(h.app.ui.toast().map(|t| t.0.as_str()), Some("✓ Discard Preview Cache"));

    assert_eq!(h.app.run("develop.autoSync", json!({"on": true})).unwrap()["on"], true);
    assert_eq!(h.app.ui.toast().map(|t| t.0.as_str()), Some("Auto Sync: On"));

    assert_eq!(h.app.run("develop.autoSync", json!({"on": false})).unwrap()["on"], false);
    assert_eq!(h.app.ui.toast().map(|t| t.0.as_str()), Some("Auto Sync: Off"));

    // This demo has no on-disk library to optimize. A failed command must not
    // report completion (or accidentally keep its preceding success toast).
    h.app.ui.toasts.clear();
    assert!(h.app.run("library.compact", json!({})).is_err());
    assert!(h.app.ui.toast().is_none());
}

/// An app on a library (in a temp folder named after `name`) with a backup due on every exit.
fn backup_due(name: &str) -> (lcc_testkit::TempDir, Headless) {
    let dir = lcc_testkit::TempDir::new(&format!("lcc-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut session = lcc_engine::Session::new().with_fs();
    session.open_library(&dir, false).unwrap();
    let settings = lcc_catalog::library::CatalogSettings {
        backup: lcc_catalog::library::BackupSchedule::EveryExit,
        last_backup: None,
        ..session.catalog_settings()
    };
    session.set_catalog_settings(&settings).unwrap();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    (dir, h)
}

/// A catalog backup due at quit asks first, as Lightroom Classic does, and says where the schedule
/// is set: Cancel keeps the app open, Skip This Time quits without one (ported from OneSlips' LR-BACKUP).
#[test]
fn quitting_with_a_backup_due_asks_and_skip_quits() {
    let (dir, mut h) = backup_due("quit-backup");
    assert!(!crate::panels::notices::may_close(&mut h.app));
    assert!(matches!(h.app.quit_prompt, Some(crate::QuitPrompt::Backup(_))), "{:?}", h.app.quit_prompt);
    h.step();
    let text = h.painted_text().join("\n");
    assert!(text.lines().any(|l| l == "Back up the catalog?"), "the prompt is titled for the catalog:\n{text}");
    assert!(text.contains("Disable backups in Edit ▸ Catalog Settings…"), "the prompt says where the schedule is set:\n{text}");
    click(&mut h, "button:quitCancel");
    assert!(h.app.quit_prompt.is_none() && !h.quit_requested(), "Cancel keeps the app open");
    assert!(!crate::panels::notices::may_close(&mut h.app), "and the next quit asks again");
    click(&mut h, "button:quitSkipBackup");
    assert_eq!(h.app.exit_backup, Some(false));
    assert!(h.quit_requested());
    assert!(crate::panels::notices::may_close(&mut h.app), "asked once");
    let _ = h.app.session.close_library();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A plug-in's wait for a program, as `plugin_programs` starts it: named, and stopping it throws the run away.
fn wait(h: &Headless) -> (lcc_engine::activity::TaskGuard, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let g = h.app.session.activity.start("pluginProgram", "Waiting for {program} to finish ({plugin})", Cancel::Flag(flag.clone()));
    g.name("program", "Converter");
    g.name("plugin", "Prog");
    g.detail("The results come back when {program} closes");
    g.warn_on_stop("What {program} makes from now on won't come back to the catalog, and {program} keeps running.");
    (g, flag)
}

fn stopped(flag: &std::sync::atomic::AtomicBool) -> bool {
    flag.load(std::sync::atomic::Ordering::Relaxed)
}

/// Stop Waiting… then the dialog's `button` (Cancel or OK): the question names the wait and says what it costs.
fn stop_waiting(h: &mut Headless, id: u64, button: &str) {
    click(h, &format!("activity:stop:{id}"));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::StopWaiting { id: d, .. }) if d == id), "it asks first");
    let text = h.painted_text().join("\n");
    assert!(text.contains("Converter keeps running"), "the question says what stopping costs:\n{text}");
    click(h, button);
    assert!(h.app.ui.dialog.is_none(), "{button} closed the question");
}

#[test]
fn a_wait_has_no_cross_and_stops_only_once_confirmed() {
    let mut h = demo();
    let (g, flag) = wait(&h);
    wait_visible(&mut h);
    let cross = format!("activity:cancel:{}", g.id());
    // the header names the program and the plug-in, with no ✕
    assert!(has(&h, PLATE));
    assert!(!has(&h, &cross), "no ✕ in the header");
    assert!(h.painted_text().iter().any(|t| t == "Waiting for Converter to finish (Prog)"), "{:?}", h.painted_text());
    // the list: no ✕, the detail says when the results come back, and Stop Waiting… asks first
    open_list(&mut h);
    assert!(!has(&h, &cross), "no ✕ in the list");
    assert!(h.painted_text().iter().any(|t| t.contains("The results come back when Converter closes")), "{:?}", h.painted_text());
    stop_waiting(&mut h, g.id(), "button:dialogCancel");
    assert!(!stopped(&flag), "Cancel leaves the run waiting");
    assert!(!h.app.session.activity.list()[0].cancelling);
    open_list(&mut h);
    stop_waiting(&mut h, g.id(), "button:dialogOk");
    assert!(stopped(&flag), "confirming stops the wait");
    // a wait that ends takes its question with it
    drop(g);
    h.step();
    h.step();
    let (g, _flag) = wait(&h);
    wait_visible(&mut h);
    open_list(&mut h);
    click(&mut h, &format!("activity:stop:{}", g.id()));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::StopWaiting { .. })), "the second wait asks too");
    drop(g);
    h.step();
    h.step();
    assert!(h.app.ui.dialog.is_none(), "nothing left to stop: the question goes");
}

/// Nothing changed since the last backup: quitting neither asks for a backup nor makes one, even
/// with one due on every exit. A change since asks again (ported from OneSlips' LR-BACKUP).
#[test]
fn quitting_with_nothing_changed_since_the_backup_does_not_ask() {
    let dir = lcc_testkit::TempDir::new("lcc-quit-backup-same");
    let _ = std::fs::remove_dir_all(&dir);
    let mut session = lcc_engine::Session::new().with_fs();
    session.open_library(&dir, false).unwrap();
    let settings = lcc_catalog::library::CatalogSettings {
        backup: lcc_catalog::library::BackupSchedule::EveryExit,
        last_backup: None,
        ..session.catalog_settings()
    };
    session.set_catalog_settings(&settings).unwrap();
    session.backup_catalog(None).unwrap();
    assert!(!session.catalog_backup_wanted(), "just backed up, nothing changed since");
    assert_eq!(session.backup_catalog_if_due().unwrap(), None, "and the exit makes none");
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    assert!(crate::panels::notices::may_close(&mut h.app), "{:?}", h.app.quit_prompt);
    assert!(!matches!(h.app.quit_prompt, Some(crate::QuitPrompt::Backup(_))));
    h.app.session.execute("album.create", &json!({"name": "Picks"})).unwrap();
    assert!(h.app.session.catalog_backup_wanted(), "a change since the backup");
    assert!(!crate::panels::notices::may_close(&mut h.app));
    assert!(matches!(h.app.quit_prompt, Some(crate::QuitPrompt::Backup(_))), "{:?}", h.app.quit_prompt);
    let _ = h.app.session.close_library();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reading a folder to browse it (the Import tab) is not the user's own work: the photos culled
/// meanwhile are prepared and their neighbours made ahead. As an "import" row it held all of that
/// back for the whole read (2026-11-09: 90 s for a wedding's 2,597 raws, the loupe never ahead).
#[test]
fn reading_a_browsed_folder_holds_no_preparation_back() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let n = lcc_engine::import::batch_size() + 4;
    let (dir, mut h) = gated_import("browse-read", n, Some(gate.clone()));
    let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(
        h.step_until(Duration::from_secs(60), |h| h.app.import.as_ref().is_some_and(|t| t.imported >= 1)),
        "the folder is being read: {:?}",
        h.app.session.activity.list()
    );
    // held at the gate until now, so the read cannot have ended
    assert!(h.app.import.is_some(), "still reading");
    let tasks = h.app.session.activity.list();
    assert!(tasks.iter().any(|t| t.label == "Reading folder"), "{tasks:?}");
    assert_eq!(h.app.session.activity.user_work(), None, "a browse's read holds nothing back: {tasks:?}");
    gate.store(true, std::sync::atomic::Ordering::Release);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.is_none()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A continuous job's row (the face scan, search indexing) has a pause button and a gear, in the Classic plate and its
/// list: pause runs the job's own command and, once the job says it is paused, the button offers resume; the gear opens
/// Settings at the job's page.
#[test]
fn a_job_with_controls_has_pause_resume_and_a_gear_on_its_row() {
    let mut h = demo();
    let controls = |paused| lcc_engine::activity::Controls {
        paused,
        pause: "ml.pause",
        resume: "ml.resume",
        settings_tab: "ai",
        settings_section: Some("semanticSearch"),
    };
    let g = h.app.session.activity.start("mlIndex", "Indexing photos", Cancel::No);
    g.set_controls(Some(controls(false)));
    wait_visible(&mut h);
    let id = g.id();
    assert!(has(&h, &format!("activity:pause:{id}")), "the plate's row has a pause button");
    assert!(has(&h, &format!("activity:settings:{id}")), "and a gear");
    assert!(!has(&h, &format!("activity:cancel:{id}")), "and no ✕");
    click(&mut h, &format!("activity:pause:{id}"));
    assert_eq!(h.app.session.execute("ml.indexStatus", &json!({})).unwrap()["paused"], true, "pause ran ml.pause");
    // the job says it is paused: the button offers resume, here and in the list
    g.set_controls(Some(controls(true)));
    h.step();
    h.step();
    assert!(has(&h, &format!("activity:resume:{id}")));
    open_list(&mut h);
    assert!(has(&h, &format!("activity:resume:{id}")), "the list's row has resume too");
    click(&mut h, &format!("activity:resume:{id}"));
    assert_eq!(h.app.session.execute("ml.indexStatus", &json!({})).unwrap()["paused"], false, "resume ran ml.resume");
    click(&mut h, &format!("activity:settings:{id}"));
    assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "ai".into() }), "the gear opened its Settings page");
    drop(g);
}
