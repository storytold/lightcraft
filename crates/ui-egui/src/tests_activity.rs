//! Issue #345: the activity stack — one place for every long-running task, with ✕ to cancel. It lives under the
//! bell in the top bar: a ring and a line beside the bell while something runs, the rows in a list a click opens.

use std::time::Duration;

use lightcraft_engine::activity::{Cancel, Ending, Failure, Outcome, TaskInfo, Unit};
use serde_json::json;

use crate::headless::Headless;
use crate::panels::activity::{WIDTH, count_text, detail_of, failures_text, overall, readout_text, summary_of, took};
use crate::{LightcraftApp, Services};

// generous: merges and renders run on worker threads that a busy machine (a full workspace test run) slows a lot
const T: Duration = Duration::from_secs(60);

fn demo() -> Headless {
    demo_sized([1200.0, 800.0])
}

fn demo_sized(size: [f32; 2]) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
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

/// The bell shows a task once it is half a second old.
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

/// Open the list under the bell (`view.tasks`) and let it lay itself out.
fn open_list(h: &mut Headless) {
    let r = h.request("engine.execute", json!({"command": "view.tasks", "params": {"show": true}}), T);
    assert_eq!(r["result"]["tasks"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(has(h, "activity:list"), "the list is open");
}

/// A task as the registry reports it, for the functions that only word or weigh one.
fn info(label: &str, done: u64, total: u64) -> TaskInfo {
    TaskInfo {
        id: 1,
        kind: "export",
        label: label.into(),
        done,
        total,
        unit: Unit::Count,
        detail: String::new(),
        cancellable: true,
        cancelling: false,
        age_ms: 600,
        waiting: false,
        place: 0,
        lane: None,
    }
}

#[test]
fn a_task_rings_the_bell_after_half_a_second_and_its_row_is_in_the_list_under_it() {
    // Given a task that has just started
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    g.progress(2, 5);
    h.step();
    // (a frame on a loaded machine can outlast the half second: only judge a task still young after it)
    if h.app.session.activity.list()[0].age_ms < crate::panels::activity::SHOW_AFTER_MS {
        assert!(!has(&h, "activity:ring"), "not before 0.5 s");
    }
    // When it is half a second old
    wait_visible(&mut h);
    // Then the bell wears a ring, and nothing is drawn over the panels
    assert!(has(&h, "activity:ring"));
    assert_eq!(rows(&h), 0, "the list is closed until the bell is clicked");
    assert_eq!(h.request("ui.inspect", json!({}), T)["result"]["activityOpen"], false);
    // When the bell is clicked
    click(&mut h, "icon:bell");
    // Then the list is under the top bar, inside the window, with the task's row
    let (list, bell) = (rect(&h, "activity:list"), rect(&h, "icon:bell"));
    assert!(list.top() >= bell.bottom() && list.right() <= 1200.0 && list.left() >= 0.0, "{list:?} under {bell:?}");
    assert!(list.contains_rect(rect(&h, &format!("activity:row:{}", g.id()))));
    assert_eq!(h.request("ui.inspect", json!({}), T)["result"]["activityOpen"], true);
    // And its ✕ asks the task to stop, with the list still open
    click(&mut h, &format!("activity:cancel:{}", g.id()));
    assert!(g.is_cancelled());
    let inspect = h.request("ui.inspect", json!({}), T);
    assert_eq!(inspect["result"]["activity"][0]["cancelling"], true, "{}", inspect["result"]["activity"]);
    assert!(has(&h, "activity:list"), "a click inside the list keeps it open");
}

#[test]
fn the_bell_opens_the_tasks_not_history_and_says_when_nothing_runs() {
    let mut h = demo();
    let right = h.app.ui.right;
    assert!(!has(&h, "activity:ring"), "no ring while nothing runs");
    click(&mut h, "icon:bell");
    assert!(has(&h, "activity:empty"), "the list says that nothing runs");
    assert_eq!(h.app.ui.right, right, "the History panel is not opened by the bell");
    // a second click closes it
    click(&mut h, "icon:bell");
    assert!(!has(&h, "activity:list"));
}

#[test]
fn many_tasks_scroll_inside_the_window_newest_first() {
    let mut h = demo();
    let guards: Vec<_> = (0..30).map(|i| h.app.session.activity.start("export", &format!("Task {i}"), Cancel::Yes)).collect();
    wait_visible(&mut h);
    open_list(&mut h);
    let list = h.view.ctx.memory(|m| m.area_rect(crate::panels::activity::popup_id())).expect("the list is drawn");
    assert!(list.bottom() <= 800.0 && list.top() >= 0.0, "the list keeps to the window and scrolls: {list:?}");
    // the task started last is at the top, the one after it below; the oldest is scrolled out of view, and a row
    // out of view offers nothing to click
    let id = |i: usize| guards[i].id();
    let (first, second) = (rect(&h, &format!("activity:row:{}", id(29))), rect(&h, &format!("activity:row:{}", id(28))));
    assert!(list.contains_rect(first) && first.bottom() <= second.top(), "{first:?} above {second:?} in {list:?}");
    assert!(rows(&h) > 3, "more rows than the three the old stack showed: {}", rows(&h));
    assert!(rows(&h) < 30 && !has(&h, &format!("activity:cancel:{}", id(0))), "rows out of view are not offered: {}", rows(&h));
    drop(guards);
    h.step();
    h.step();
    assert_eq!(rows(&h), 0);
    assert!(!has(&h, "activity:ring"), "the ring goes with the last task");
}

#[test]
fn not_cancellable_rows_have_no_cross() {
    let mut h = demo();
    let g = h.app.session.activity.start("faces", "Finding faces", Cancel::No);
    wait_visible(&mut h);
    open_list(&mut h);
    assert!(has(&h, &format!("activity:row:{}", g.id())));
    assert!(!has(&h, &format!("activity:cancel:{}", g.id())));
}

#[test]
fn count_text_by_unit() {
    let mut t = info("Exporting", 3, 25);
    assert_eq!(count_text(&t), "3 of 25");
    (t.done, t.total, t.unit) = (12 * 1_048_576, 340 * 1_048_576, Unit::Bytes);
    assert_eq!(count_text(&t), "12 of 340 MB");
    (t.done, t.total, t.unit) = (400, 1000, Unit::Percent);
    assert_eq!(count_text(&t), "40 %");
    t.total = 0;
    assert_eq!(count_text(&t), "");
}

#[test]
fn the_line_beside_the_bell_names_one_task_or_counts_several() {
    assert_eq!(readout_text(&[], 0), "");
    assert_eq!(readout_text(&[info("Exporting", 10, 25)], 0), "Exporting · 40 %");
    assert_eq!(readout_text(&[info("Finding faces", 0, 0)], 0), "Finding faces", "nothing to say about how far while that isn't known");
    let stopping = TaskInfo { cancelling: true, ..info("Exporting", 10, 25) };
    assert_eq!(readout_text(&[stopping], 0), "Exporting · Stopping…");
    assert_eq!(readout_text(&[info("Exporting", 1, 2), info("Importing", 0, 0), info("Merging", 3, 4)], 0), "3 tasks");
    // those waiting in line are counted after what runs, and alone in the moment before the next one starts
    assert_eq!(readout_text(&[info("Building previews", 1, 4)], 2), "Building previews · 25 % · 2 waiting");
    assert_eq!(readout_text(&[info("Finding faces", 0, 0)], 1), "Finding faces · 1 waiting");
    assert_eq!(readout_text(&[], 1), "1 waiting");
}

#[test]
fn a_long_name_is_cut_short_before_the_percentage_is() {
    // Given a wide window and a task whose name alone is longer than the line beside the bell may be
    let mut h = demo_sized([1600.0, 900.0]);
    let long = h.app.session.activity.start("lightroom", "Reading the Lightroom catalog of the whole archive", Cancel::Yes);
    long.progress(2, 5);
    wait_visible(&mut h);
    let wide = rect(&h, "activity:readout");
    drop(long);
    h.step();
    // When a task with a short name is as far along
    let short = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    short.progress(2, 5);
    wait_visible(&mut h);
    let narrow = rect(&h, "activity:readout");
    // Then the long one took more room but kept to the line's limit: its name gave way, the line is still there
    assert!(wide.width() > narrow.width() && wide.width() <= 221.0, "{wide:?} / {narrow:?}");
    // And as the window narrows, the line never grows; before it goes there is a width at which only the
    // percentage is left (narrower than any name is allowed to be), and once gone it stays gone
    let long = h.app.session.activity.start("lightroom", "Reading the Lightroom catalog of the whole archive", Cancel::Yes);
    drop(short);
    long.progress(2, 5);
    wait_visible(&mut h);
    let widths: Vec<Option<f32>> = (640..=1600).rev().step_by(20).map(|w| readout_width_at(&mut h, w as f32)).collect();
    for pair in widths.windows(2) {
        let (wider, narrower) = (pair[0].unwrap_or(0.0), pair[1].unwrap_or(0.0));
        assert!(narrower <= wider + 0.5, "the line grew as the window narrowed: {widths:?}");
    }
    assert!(widths.iter().flatten().any(|w| *w < 56.0), "at some width only the percentage is left: {widths:?}");
    assert_eq!(widths.last(), Some(&None), "a narrow window has the ring alone: {widths:?}");
}

/// The width of the line beside the bell in a window `width` wide (`None` when it is left out).
fn readout_width_at(h: &mut Headless, width: f32) -> Option<f32> {
    assert_eq!(h.request("ui.resize", json!({"width": width, "height": 800.0}), T)["ok"], true);
    for _ in 0..3 {
        h.step();
    }
    h.app.widgets.iter().find(|(w, _)| w == "activity:readout").map(|(_, r)| r.width())
}

#[test]
fn the_ring_shows_the_mean_of_the_tasks_whose_work_is_known() {
    assert_eq!(overall(&[]), None);
    assert_eq!(overall(&[info("Finding faces", 0, 0)]), None, "the ring sweeps while no amount is known");
    assert_eq!(overall(&[info("Exporting", 1, 4)]), Some(0.25));
    assert_eq!(overall(&[info("Exporting", 1, 4), info("Importing", 3, 4), info("Finding faces", 0, 0)]), Some(0.5));
    assert_eq!(overall(&[info("Exporting", 9, 4)]), Some(1.0), "never past the whole ring");
}

#[test]
fn the_bar_names_the_task_where_it_has_room_and_counts_on_the_bell_where_it_has_not() {
    // Given a wide window and one task
    let mut h = demo_sized([1600.0, 900.0]);
    let a = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    a.progress(2, 5);
    wait_visible(&mut h);
    // Then a line beside the bell names it, between the filter icon and the bell
    let (line, bell, filter) = (rect(&h, "activity:readout"), rect(&h, "icon:bell"), rect(&h, "icon:filter"));
    assert!(line.right() <= bell.left() && line.left() >= filter.right(), "{line:?} between {filter:?} and {bell:?}");
    assert!(!has(&h, "activity:count"), "one task needs no count");
    // And a click on the line opens the list, as one on the bell does
    click(&mut h, "activity:readout");
    assert!(has(&h, "activity:list"));
    // Given a window with no room beside the bell, and two tasks
    let mut h = demo();
    let _a = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    let _b = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    wait_visible(&mut h);
    assert_eq!(readout_width_at(&mut h, 640.0), None);
    // Then the line is left out, the ring stays, and the bell says how many
    assert!(!has(&h, "activity:readout"));
    assert!(has(&h, "activity:ring") && has(&h, "activity:count"));
    // And with one task left, the ring alone: there is nothing to count
    drop(_b);
    h.step();
    h.step();
    assert!(has(&h, "activity:ring") && !has(&h, "activity:count") && !has(&h, "activity:readout"));
}

#[test]
fn escape_closes_the_list_and_nothing_else() {
    let mut h = demo();
    h.request("engine.execute", json!({"command": "view.detail", "params": {}}), T);
    h.step();
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail);
    open_list(&mut h);
    // Esc closes the list; the command Esc has otherwise (Back to Grid) waits for the next press
    assert_eq!(h.request("ui.key", json!({"key": "Escape"}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    assert!(!has(&h, "activity:list") && !h.app.activity_open, "Esc closes the list");
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail, "and does nothing else");
}

#[test]
fn a_click_elsewhere_or_on_the_bell_again_closes_the_list() {
    let mut h = demo_sized([1600.0, 900.0]);
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    g.progress(2, 5);
    wait_visible(&mut h);
    // open by the line beside the bell, closed by a second click on it
    click(&mut h, "activity:readout");
    assert!(has(&h, "activity:list"));
    click(&mut h, "activity:readout");
    assert!(!has(&h, "activity:list"), "the line toggles the list, as the bell does");
    // open again, closed by a click on something else
    click(&mut h, "icon:bell");
    assert!(has(&h, "activity:list"));
    click(&mut h, "icon:filter");
    assert!(!has(&h, "activity:list") && !h.app.activity_open, "a click elsewhere closes it");
    assert!(!g.is_cancelled(), "closing the list stops nothing");
}

#[test]
fn the_list_gives_way_to_a_dialog_and_view_tasks_says_why_it_cannot_open() {
    let mut h = demo();
    open_list(&mut h);
    h.request("engine.execute", json!({"command": "app.shortcuts", "params": {}}), T);
    for _ in 0..4 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_some(), "a dialog is open");
    assert!(!has(&h, "activity:list") && !h.app.activity_open, "the list gave way to the dialog");
    // asked for over the dialog, it is refused with the reason, and nothing opens behind the dialog or after it
    let r = h.request("engine.execute", json!({"command": "view.tasks", "params": {"show": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().is_some_and(|e| e.contains("a dialog is open")), "{r}");
    h.app.ui.dialog = None;
    for _ in 0..4 {
        h.step();
    }
    assert!(!has(&h, "activity:list"), "the refused request is not kept for later");
}

#[test]
fn the_list_gives_way_to_the_question_before_quitting() {
    let mut h = demo();
    let _g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    open_list(&mut h);
    assert!(!crate::panels::notices::may_close(&mut h.app));
    for _ in 0..4 {
        h.step();
    }
    assert!(has(&h, "button:quitCancel"), "the question is asked");
    assert!(!has(&h, "activity:list") && !h.app.activity_open, "and the list is not left open over it");
    let r = h.request("engine.execute", json!({"command": "view.tasks", "params": {"show": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
}

#[test]
fn full_screen_preview_closes_the_list_and_keeps_no_request_for_later() {
    let mut h = demo();
    h.request("engine.execute", json!({"command": "view.detail", "params": {}}), T);
    open_list(&mut h);
    // Given the list open, When the photo goes full screen (no top bar)
    h.app.ui.fullscreen = true;
    for _ in 0..3 {
        h.step();
    }
    // Then the list is closed, and said to be
    assert!(!has(&h, "activity:list") && !h.app.activity_open);
    assert_eq!(h.request("ui.inspect", json!({}), T)["result"]["activityOpen"], false);
    // And asking for it there is refused with the reason, not kept until full screen ends
    let r = h.request("engine.execute", json!({"command": "view.tasks", "params": {"show": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().is_some_and(|e| e.contains("Full Screen Preview")), "{r}");
    h.app.ui.fullscreen = false;
    for _ in 0..4 {
        h.step();
    }
    assert!(!has(&h, "activity:list"), "nothing pops open on the way back");
}

#[test]
fn a_task_just_started_has_its_row_in_an_open_list_at_once() {
    let mut h = demo();
    open_list(&mut h);
    assert!(has(&h, "activity:empty"));
    // the bell waits half a second so quick tasks don't flash; a list that is open never says "none" over a task
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    h.step();
    h.step();
    assert!(has(&h, &format!("activity:row:{}", g.id())) && !has(&h, "activity:empty"));
}

#[test]
fn view_tasks_opens_and_closes_the_list() {
    let mut h = demo();
    open_list(&mut h);
    // without `show` it toggles
    let r = h.request("engine.execute", json!({"command": "view.tasks", "params": {}}), T);
    assert_eq!(r["result"]["tasks"], false, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(!has(&h, "activity:list"));
    let r = h.request("engine.execute", json!({"command": "view.tasks", "params": {}}), T);
    assert_eq!(r["result"]["tasks"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(has(&h, "activity:list"));
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
fn long_detail_stays_inside_the_list() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exportieren", Cancel::Yes);
    g.detail(&"IMG_".repeat(75));
    wait_visible(&mut h);
    open_list(&mut h);
    let row = rect(&h, &format!("activity:row:{}", g.id()));
    assert!(row.width() <= WIDTH + 1.0, "{row:?}");
}

#[test]
fn cross_sits_at_the_right_edge_of_its_row() {
    let mut h = demo();
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    wait_visible(&mut h);
    open_list(&mut h);
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
fn dead_export_worker_leaves_no_running_row() {
    let mut h = demo();
    h.app.services.write_shared = Some(std::sync::Arc::new(|_: &str, _: &[u8]| -> Result<(), String> { panic!("synthetic writer panic") }));
    start_export(&mut h, 2);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
    assert!(h.app.session.activity.list().is_empty());
}

/// A folder of `n` stand-in JPEGs and an app whose file probe takes 100 ms each (a slow drive).
fn slow_import(tag: &str, n: usize) -> (Headless, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-activity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..n {
        std::fs::write(dir.join(format!("IMG_{i:03}.jpg")), format!("not really a jpeg {i}")).unwrap();
    }
    let mut s = lightcraft_engine::Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(|p: &str| {
        std::thread::sleep(Duration::from_millis(100));
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
    // and, once the toast has passed, it is still in the list under the bell, saying how far it got
    let rows = h.app.session.activity.finished();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].ending), ("import", Ending::Stopped));
    assert!(rows[0].summary.starts_with("Import cancelled"), "{}", rows[0].summary);
    assert!(h.app.session.catalog.len() < n, "stopped part-way");
    assert_eq!(h.app.session.undo.len(), undo0 + 1, "one undo step");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A photo chosen while an import runs stays the active one when the import ends: the import
/// selects its first photo only if nobody picked something else meanwhile (issue #1446).
#[test]
fn a_photo_chosen_during_an_import_stays_selected() {
    let (mut h, dir) = slow_import("chosen", 20);
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.as_ref().is_some_and(|t| t.imported >= 2)));
    // the newest photo so far, never the import's first
    let chosen = h.app.session.catalog.photos().map(|p| p.id).max().expect("photos arrived");
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [chosen.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.import.is_some(), "still importing");
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
    let row = h.app.session.activity.last_finished().expect("a stopped scan leaves a row");
    assert_eq!((row.label.as_str(), row.ending), ("Scanning folder", Ending::Stopped));
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
    let row = h.app.session.activity.last_finished().expect("a stopped merge leaves a row");
    assert_eq!((row.ending, row.summary.as_str()), (Ending::Stopped, "HDR merge cancelled"));
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
    // different exposures, or the import would take set B for duplicates of set A
    for (set, evs) in [("A", [-2.0, 0.0, 2.0]), ("B", [-1.5, 0.5, 2.5]), ("C", [-1.0, 1.0, 3.0])] {
        for (i, bytes) in lightcraft_merge::synth::bracket_dngs(1600, 1200, &evs).unwrap().into_iter().enumerate() {
            let p = dir.join(format!("{set}_{i}.dng"));
            std::fs::write(&p, bytes).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
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
    let mut h = demo();
    h.app.session.activity.keep_after(0);
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
    let row = h.app.session.activity.last_finished().expect("and a row says what it found");
    assert_eq!(row.ending, Ending::Done);
    assert!(row.summary.starts_with("Found 0 missing photos"), "{:?}", row.summary);
    let _ = std::fs::remove_dir_all(dir);
}

/// A Lightroom catalog that can't be read leaves a row saying the task failed, and why.
#[test]
fn a_lightroom_catalog_that_cannot_be_read_leaves_a_failed_row() {
    let mut h = demo();
    let dir = std::env::temp_dir().join(format!("lc-activity-lrcat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Not a catalog.lrcat");
    std::fs::write(&path, b"this is not a Lightroom catalog").unwrap();
    let r = crate::menus::run_ui_command(&mut h.app, "library.inspectLightroom", &json!({"path": path.to_string_lossy(), "wait": true}));
    assert!(r.is_some(), "the command is the interface's");
    let row = h.app.session.activity.last_finished().expect("a failed task always leaves a row");
    assert_eq!((row.kind, row.ending), ("lightroom", Ending::Failed));
    assert!(row.summary.starts_with("Lightroom inspection failed"), "{:?}", row.summary);
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
fn rows_added_later_grow_the_open_list() {
    let mut h = demo();
    let a = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    a.progress(1, 24);
    a.detail("LC01347.jpg");
    wait_visible(&mut h);
    open_list(&mut h);
    // two more tasks while the list already shows one row: it grows to show them, nothing is cut off
    let b = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    let c = h.app.session.activity.start("lightroom", "Reading Lightroom catalog", Cancel::Yes);
    wait_visible(&mut h);
    for _ in 0..5 {
        h.step();
    }
    let list = h.view.ctx.memory(|m| m.area_rect(crate::panels::activity::popup_id())).expect("the list is drawn");
    for g in [&a, &b, &c] {
        let r = rect(&h, &format!("activity:row:{}", g.id()));
        assert!(list.contains_rect(r), "row {} at {r:?} is cut off by the list at {list:?}", g.id());
    }
}

#[test]
fn immediate_library_commands_and_auto_sync_show_completion() {
    let mut h = demo();
    assert!(h.app.run("library.clearPreviews", json!({})).is_ok());
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("✓ Clear Preview Cache"));

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

/// A task that ended `ending`, kept in the list whatever time it took. Its id.
fn ended(h: &mut Headless, label: &str, ending: Ending, summary: &str) -> u64 {
    h.app.session.activity.keep_after(0);
    let g = h.app.session.activity.start("export", label, Cancel::Yes);
    let id = g.id();
    assert!(g.finish(Outcome { ending, summary: summary.into(), ..Default::default() }));
    id
}

#[test]
fn a_task_that_ended_stays_in_the_list_until_dismissed() {
    let mut h = demo();
    // Given an export that ran long and ended well, and nothing running
    let id = ended(&mut h, "Exporting", Ending::Done, "Exported 5,000 photos");
    h.step();
    h.step();
    // Then the bell asks for nothing: no ring, no mark
    assert!(!has(&h, "activity:ring") && !has(&h, "activity:attention"));
    // When the list is opened, Then the task is there, and "No tasks are running" is not said over it
    open_list(&mut h);
    assert!(has(&h, &format!("activity:done:{id}")) && !has(&h, "activity:empty"));
    let inspect = h.request("ui.inspect", json!({}), T);
    assert_eq!(inspect["result"]["activityFinished"][0]["summary"], "Exported 5,000 photos", "{inspect}");
    // And it stays, frame after frame
    for _ in 0..20 {
        h.step();
    }
    assert!(has(&h, &format!("activity:done:{id}")));
    // When its ✕ is clicked, Then it is gone, and the list is still open
    click(&mut h, &format!("activity:dismiss:{id}"));
    assert!(h.app.session.activity.finished().is_empty());
    assert!(has(&h, "activity:empty"), "the list stays open on what is left");
}

#[test]
fn a_task_that_did_not_end_well_marks_the_bell_until_its_row_is_dismissed() {
    let mut h = demo();
    let id = ended(&mut h, "Exporting", Ending::Failed, "The disk is full");
    let other = ended(&mut h, "Importing", Ending::Stopped, "Import cancelled");
    h.step();
    h.step();
    // the mark is on the bell while the list is closed, however many frames pass
    assert!(has(&h, "activity:attention"));
    for _ in 0..10 {
        h.step();
    }
    assert!(has(&h, "activity:attention"));
    // looking at the list does not answer it: the mark stays while the list is open, and after it is closed
    open_list(&mut h);
    assert!(has(&h, "activity:attention") && has(&h, &format!("activity:done:{id}")));
    click(&mut h, "icon:bell");
    assert!(!has(&h, "activity:list") && has(&h, "activity:attention"), "seen is not dealt with");
    // dismissing one of two leaves the mark; dismissing the last takes it away
    open_list(&mut h);
    click(&mut h, &format!("activity:dismiss:{id}"));
    assert!(has(&h, "activity:attention") && h.app.session.activity.problems() == 1);
    click(&mut h, &format!("activity:dismiss:{other}"));
    assert!(!has(&h, "activity:attention") && h.app.session.activity.problems() == 0);
}

#[test]
fn rows_are_grouped_by_what_they_need_and_clear_finished_lets_go_of_the_ended_ones() {
    let mut h = demo();
    let bad = ended(&mut h, "Importing", Ending::Problems, "Imported 90 photos");
    let good = ended(&mut h, "Exporting", Ending::Done, "Exported 24 photos");
    let running = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    wait_visible(&mut h);
    open_list(&mut h);
    // what needs looking at first, then what runs, then what ended well
    let y = |id: &str| rect(&h, id).top();
    let (bad_y, run_y, good_y) =
        (y(&format!("activity:done:{bad}")), y(&format!("activity:row:{}", running.id())), y(&format!("activity:done:{good}")));
    assert!(bad_y < run_y && run_y < good_y, "{bad_y} < {run_y} < {good_y}");
    // Clear finished lets go of both ended tasks and leaves the running one
    click(&mut h, "activity:clear");
    assert!(h.app.session.activity.finished().is_empty());
    assert!(has(&h, &format!("activity:row:{}", running.id())) && !has(&h, "activity:clear"));
    assert!(!has(&h, "activity:attention"), "and with them the mark on the bell");
    assert!(!running.is_cancelled(), "clearing stops nothing");
}

#[test]
fn a_task_that_just_ended_is_named_beside_the_bell_for_a_moment() {
    let mut h = demo_sized([1600.0, 900.0]);
    ended(&mut h, "Exporting", Ending::Done, "Exported 24 photos");
    h.step();
    h.step();
    assert!(has(&h, "activity:readout"), "the line says how it ended");
    // after a few seconds the line is gone; the row is not
    std::thread::sleep(Duration::from_millis(6_100));
    h.step();
    h.step();
    assert!(!has(&h, "activity:readout") && h.app.session.activity.finished().len() == 1);
}

#[test]
fn an_ended_task_is_worded_from_its_name_when_it_said_nothing_and_says_how_long_it_took() {
    let mut h = demo();
    let id = ended(&mut h, "Building previews", Ending::Died, "");
    let rows = h.app.session.activity.finished();
    let row = rows.iter().find(|f| f.id == id).unwrap();
    assert_eq!(summary_of(row), "Building previews: stopped unexpectedly");
    assert_eq!(
        (took(42_900).as_str(), took(12 * 60_000 + 5_000).as_str(), took(6_720_000).as_str()),
        ("took 42 s", "took 12 min", "took 1 h 52 min")
    );
}

#[test]
fn an_export_that_failed_for_a_file_leaves_a_row_with_the_file_and_the_reason() {
    let mut h = demo();
    // Given a disk that refuses the first file of five
    let refuse = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    h.app.services.write_shared =
        Some(std::sync::Arc::new(
            move |_: &str, _: &[u8]| {
                if refuse.swap(false, std::sync::atomic::Ordering::Relaxed) { Err("The disk is full".into()) } else { Ok(()) }
            },
        ));
    // When five photos are exported
    start_export(&mut h, 5);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
    // Then the export is in the list, however short it was: done, with problems, in its folder
    let rows = h.app.session.activity.finished();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!((row.kind, row.ending, row.failed), ("export", Ending::Problems, 1), "{row:?}");
    assert_eq!(row.summary, "Exported 4 of 5 photos");
    assert!(row.path.as_deref().is_some_and(|p| p.starts_with("/lc-test-out/")), "one of the files written: {:?}", row.path);
    assert!(!row.failures[0].name.is_empty() && row.failures[0].reason.contains("The disk is full"), "{:?}", row.failures);
    // And it is said at once too, as ever; the bell is marked for when the toast has passed
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Exported 4 of 5")), "{:?}", h.app.ui.toast);
    h.step();
    assert!(has(&h, "activity:attention"));
    // When the row's failed files are asked for, Then the file and the reason can be copied
    open_list(&mut h);
    click(&mut h, &format!("activity:failures:{}", row.id));
    click(&mut h, &format!("activity:copy:{}", row.id));
    let want = format!("{}\t{}", row.failures[0].name, row.failures[0].reason);
    assert!(h.step_until(T, |h| h.app.copied.as_deref() == Some(want.as_str())), "{:?}", h.app.copied);
}

#[test]
fn a_short_clean_export_is_a_toast_and_leaves_no_row() {
    let mut h = demo();
    // (short by any machine's clock: the ten seconds a row is kept after are far away here)
    h.app.session.activity.keep_after(600_000);
    h.app.services.write_shared = Some(slow_writer(1));
    start_export(&mut h, 2);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
    assert!(h.app.session.activity.finished().is_empty(), "watched to its end: nothing to come back to");
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Exported 2 of 2")), "{:?}", h.app.ui.toast);
}

#[test]
fn a_stopped_export_and_a_dead_export_worker_each_leave_a_row() {
    let mut h = demo();
    h.app.services.write_shared = Some(slow_writer(200));
    start_export(&mut h, 5);
    assert_eq!(h.app.session.activity.cancel_all(), 1);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
    let rows = h.app.session.activity.finished();
    assert_eq!(rows.iter().map(|f| f.ending).collect::<Vec<_>>(), vec![Ending::Stopped], "{rows:?}");
    // a writer that panics takes the worker with it: the export says so instead of vanishing
    h.app.services.write_shared = Some(std::sync::Arc::new(|_: &str, _: &[u8]| -> Result<(), String> { panic!("synthetic writer panic") }));
    start_export(&mut h, 2);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
    let rows = h.app.session.activity.finished();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!((rows[0].ending, rows[0].summary.as_str()), (Ending::Died, "Export stopped unexpectedly"), "{:?}", rows[0]);
}

#[test]
fn the_failed_files_copied_say_how_many_were_not_kept_and_stay_a_line_each() {
    let h = demo();
    h.app.session.activity.keep_after(0);
    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    let failures = vec![
        Failure { name: "IMG_1.jpg".into(), reason: "The disk\nis full".into() },
        Failure { name: "IMG\t2.jpg".into(), reason: "No permission".into() },
    ];
    // a job that counted 700 failures and could name two of them
    g.finish(Outcome { ending: Ending::Problems, failures, failed: 700, ..Default::default() });
    let row = &h.app.session.activity.finished()[0];
    assert_eq!(failures_text(row), "IMG_1.jpg\tThe disk is full\nIMG 2.jpg\tNo permission\nand 698 more");
}

/// A task that only counted what it could not do (a preview build) has the count said under its name; one that said
/// more keeps what it said.
#[test]
fn an_ended_task_that_only_counted_its_failures_says_how_many() {
    let h = demo();
    h.app.session.activity.keep_after(0);
    let g = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    g.finish(Outcome { ending: Ending::Problems, failed: 3, ..Default::default() });
    let f = h.app.session.activity.last_finished().unwrap();
    assert_eq!((summary_of(&f), detail_of(&f)), ("Building previews: done, with problems".to_string(), "3 failed".to_string()));
    let g = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    g.finish(Outcome { ending: Ending::Problems, failed: 2, detail: "2 not readable".into(), ..Default::default() });
    assert_eq!(detail_of(&h.app.session.activity.last_finished().unwrap()), "2 not readable");
}

/// A scan that ran to its end says so, and says when it found nothing to import.
#[test]
fn a_scan_that_found_nothing_leaves_a_row_saying_so() {
    let mut h = demo();
    h.app.session.activity.keep_after(0);
    let dir = std::env::temp_dir().join(format!("lc-activity-emptyscan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    crate::import::open(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    assert!(h.step_until(T, |h| h.app.scan.is_none()));
    let row = h.app.session.activity.last_finished().expect("a row for the scan");
    assert_eq!((row.label.as_str(), row.ending, row.detail.as_str()), ("Scanning folder", Ending::Done, "No photos found"));
    let _ = std::fs::remove_dir_all(dir);
}

/// The menu item of `command` whose params have `size`, anywhere in the menu tree (`ui.menu.tree`).
fn menu_item(tree: &serde_json::Value, command: &str, size: &str) -> Option<serde_json::Value> {
    match tree {
        serde_json::Value::Array(nodes) => nodes.iter().find_map(|n| menu_item(n, command, size)),
        serde_json::Value::Object(node) => {
            let found = node.get("id").and_then(|v| v.as_str()) == Some(command) && node.get("params").is_some_and(|p| p["size"] == size);
            if found { Some(tree.clone()) } else { node.values().find_map(|v| menu_item(v, command, size)) }
        }
        _ => None,
    }
}

/// Given a preview build running, then Build Standard-Sized Previews is still offered. When it is chosen, then it
/// waits in line: a toast says so, and the list shows it under the running build, starting next. When its ✕ is
/// clicked, then it leaves the line, and the running build goes on.
#[test]
fn a_second_preview_build_waits_in_line_and_can_be_taken_out_of_it() {
    let mut h = demo();
    let build = std::sync::Arc::new(lightcraft_engine::cmd::previews::PreviewBuild::default());
    h.app.session.preview_build = Some(build.clone());
    let running = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    let tree = h.request("ui.menu.tree", json!({}), T)["result"].clone();
    let item = menu_item(&tree, "library.buildPreviews", "standard").expect("Build Standard-Sized Previews in the menus");
    assert_eq!(item["enabled"], true, "not greyed out while a build runs: {item}");
    let r = h.request("ui.menu.invoke", json!({"id": "library.buildPreviews", "params": item["params"]}), T);
    let id = r["result"]["queued"].as_u64().unwrap_or_else(|| panic!("it waits in line: {r}"));
    assert!(h.app.ui.toast.as_ref().is_some_and(|(text, ..)| text.starts_with("Queued")), "{:?}", h.app.ui.toast);
    open_list(&mut h);
    let (waiting, ahead) = (rect(&h, &format!("activity:row:{id}")), rect(&h, &format!("activity:row:{}", running.id())));
    assert!(waiting.top() > ahead.bottom(), "the waiting build is listed under the running one");
    click(&mut h, &format!("activity:cancel:{id}"));
    assert!(h.app.session.activity.waiting().is_empty(), "✕ took it out of the line");
    assert!(!running.is_cancelled(), "and left the running build alone");
    build.finished.store(true, std::sync::atomic::Ordering::Release);
    drop(running);
}

/// Given a preview build that did not go well, then its row offers Retry. When Retry is clicked, then the row goes and
/// the build runs again, for the same photos.
#[test]
fn a_build_that_did_not_go_well_offers_retry() {
    let mut h = demo();
    let photo = h.app.session.catalog.photos().next().map(|p| p.id.0).unwrap();
    let g = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    let request = lightcraft_engine::activity::Request {
        command: "library.buildPreviews".into(),
        params: json!({"ids": [photo], "edge": 256}),
        ..Default::default()
    };
    g.set_request(request);
    let failed = g.id();
    g.finish(Outcome::failed("The disk is full"));
    open_list(&mut h);
    click(&mut h, &format!("activity:retry:{failed}"));
    assert!(h.app.session.activity.finished().iter().all(|f| f.id != failed), "the row was taken up by the retry");
    let progress = h.app.session.execute("library.previewProgress", &json!({})).unwrap();
    assert_eq!(progress["total"], 1, "a build of that one photo started: {progress}");
    assert!(h.step_until(T, |h| h.app.session.activity.list().is_empty()));
}

/// Given a preview build running and another waiting for it, when Quit is chosen, then the question says that one
/// runs and one won't run; Quit Anyway stops the one and empties the line.
#[test]
fn quitting_says_how_many_tasks_waiting_in_line_would_not_run() {
    let mut h = demo();
    let build = std::sync::Arc::new(lightcraft_engine::cmd::previews::PreviewBuild::default());
    h.app.session.preview_build = Some(build.clone());
    let running = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    let r = h.app.run("library.buildPreviews", json!({"edge": 256, "queue": true}));
    assert!(r.as_ref().is_ok_and(|v| v["place"] == 1), "{r:?}");
    assert!(!crate::panels::notices::may_close(&mut h.app));
    let Some(crate::QuitPrompt::Tasks(text)) = h.app.quit_prompt.clone() else { panic!("{:?}", h.app.quit_prompt) };
    assert_eq!(text, "1 task is still running: Building previews. Quitting stops it. 1 task waiting in line won't run.");
    // (what Quit Anyway does)
    h.app.session.activity.cancel_all();
    assert!(running.is_cancelled() && h.app.session.activity.waiting().is_empty());
    build.finished.store(true, std::sync::atomic::Ordering::Release);
    drop(running);
}

/// Given a preview build seen running, when the next one in line takes its place before a frame saw it end, then
/// its end is still announced.
#[test]
fn the_end_of_a_build_replaced_by_the_next_in_line_is_still_announced() {
    use lightcraft_engine::cmd::previews::PreviewBuild;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    let mut h = demo();
    let first = Arc::new(PreviewBuild { total: 3, ..Default::default() });
    first.done.store(3, Ordering::Relaxed);
    h.app.session.preview_build = Some(first.clone());
    h.step();
    // it ends, and the next one starts, between two frames
    first.finished.store(true, Ordering::Release);
    h.app.session.preview_build = Some(Arc::new(PreviewBuild { total: 5, ..Default::default() }));
    h.app.ui.toast = None;
    h.step();
    assert!(h.app.ui.toast.as_ref().is_some_and(|(text, ..)| text == "Previews ready for 3 photos"), "{:?}", h.app.ui.toast);
}
