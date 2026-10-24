//! Tasks waiting in line for their lane (see `crate::activity`), with the preview builds.
//!
//! Scenarios, in the words of an agent building previews for several shoots:
//!
//! * Build Previews while a build runs: without `queue` it is refused, as it always was. With `queue` it waits
//!   behind the build, `activity.list` says it is next, and it builds the photos chosen when it was asked.
//! * It starts by itself when the build ahead of it ends.
//! * `activity.cancel` on it takes it out of the line: it never runs; `all` empties the line.
//! * The line is bounded.
//! * Another library: nothing waits for it.
//!
//! These go through commands and `activity.list` only, so that they describe what a control or MCP client sees.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use crate::Session;
use crate::activity::{Cancel, MAX_WAITING, TaskGuard};
use crate::cmd::previews::PreviewBuild;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-task-queue-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A preview build that holds the lane until [`release`]: its row runs, and the build reads as unfinished.
struct Hold {
    _row: TaskGuard,
    build: Arc<PreviewBuild>,
}

fn hold(s: &mut Session) -> Hold {
    let build = Arc::new(PreviewBuild::default());
    s.preview_build = Some(build.clone());
    Hold { _row: s.activity.start("previews", "Building previews", Cancel::Yes), build }
}

/// The held build ends as a real one does: finished first, then its row goes.
fn release(h: Hold) {
    h.build.finished.store(true, Ordering::Release);
    drop(h);
}

/// Wait (at most a minute) until no task runs or waits.
fn settle(s: &mut Session) {
    let t0 = std::time::Instant::now();
    loop {
        // any command lets a waiting task whose lane is free start (the app's frame loop does too)
        let rows = tasks(s);
        if rows.is_empty() {
            return;
        }
        assert!(t0.elapsed().as_secs() < 60, "tasks still listed: {rows:?}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn tasks(s: &mut Session) -> Vec<Value> {
    s.execute("activity.list", &json!({})).unwrap()["tasks"].as_array().cloned().unwrap_or_default()
}

fn build_ids(s: &Session, n: usize) -> Vec<u64> {
    s.catalog.photos().take(n).map(|p| p.id.0).collect()
}

/// Given a preview build running and two photos chosen, when Build Previews is asked for with `queue`, then it waits
/// behind the build, first in line, with nothing done. When other photos are chosen meanwhile and the build ahead
/// ends, then it starts by itself and builds the two photos chosen when it was asked.
#[test]
fn a_build_asked_for_while_one_runs_waits_behind_it_and_builds_the_photos_chosen_then() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 3);
    s.execute("library.select", &json!({"ids": [ids[0], ids[1]]})).unwrap();
    let h = hold(&mut s);
    let r = s.execute("library.buildPreviews", &json!({"edge": 256, "queue": true})).unwrap();
    let id = r["queued"].as_u64().expect("an id while it waits");
    assert_eq!(r["place"], 1);
    let rows = tasks(&mut s);
    assert_eq!(rows.len(), 2, "the running build, then the waiting one: {rows:?}");
    assert_eq!(rows[1]["id"], id);
    assert_eq!((rows[1]["waiting"].as_bool(), rows[1]["place"].as_u64(), rows[1]["lane"].as_str()), (Some(true), Some(1), Some("previews")));
    assert_eq!((rows[1]["done"].as_u64(), rows[1]["total"].as_u64()), (Some(0), Some(0)));
    assert_eq!(rows[1]["label"], "Building previews");
    assert_eq!((rows[0]["waiting"].as_bool(), rows[0]["place"].as_u64()), (Some(false), Some(0)), "the running one is not in line");
    s.execute("library.select", &json!({"ids": [ids[2]]})).unwrap();
    release(h);
    settle(&mut s);
    let progress = s.execute("library.previewProgress", &json!({})).unwrap();
    assert_eq!((progress["total"].as_u64(), progress["done"].as_u64()), (Some(2), Some(2)), "{progress}");
    // the journal has the call that asked for it, once: starting it was not a second call
    assert_eq!(s.journal.iter().filter(|(c, _)| c == "library.buildPreviews").count(), 1, "{:?}", s.journal);
}

/// Given a preview build running, when a build is asked for without `queue`, then it is refused as before, and
/// nothing waits.
#[test]
fn a_build_asked_for_while_one_runs_is_refused_unless_it_asks_to_queue() {
    let mut s = Session::with_demo();
    let h = hold(&mut s);
    let e = s.execute("library.buildPreviews", &json!({"edge": 256})).unwrap_err().to_string();
    assert!(e.contains("a preview build is already running"), "{e}");
    let e = s.execute("library.buildPreviews", &json!({"edge": 256, "queue": false})).unwrap_err().to_string();
    assert!(e.contains("a preview build is already running"), "{e}");
    assert_eq!(tasks(&mut s).len(), 1, "only the running build is listed");
    release(h);
    assert!(tasks(&mut s).is_empty());
}

/// Given no build running, when a build is asked for with `queue`, then it starts at once, as one without it does.
#[test]
fn a_build_that_asks_to_queue_starts_at_once_when_nothing_is_ahead_of_it() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 2);
    let r = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true, "wait": true})).unwrap();
    assert!(r.get("queued").is_none(), "{r}");
    assert_eq!((r["total"].as_u64(), r["done"].as_u64()), (Some(2), Some(2)), "{r}");
}

/// Given a build waiting behind another, when `activity.cancel` is asked for it, then it leaves the line and never
/// runs.
#[test]
fn a_waiting_build_taken_out_of_the_line_never_runs() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    let held = h.build.clone();
    let id = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap()["queued"].as_u64().unwrap();
    assert_eq!(s.execute("activity.cancel", &json!({"id": id})).unwrap()["cancelled"], 1);
    assert_eq!(tasks(&mut s).len(), 1, "only the running build is left");
    assert!(s.execute("activity.cancel", &json!({"id": id})).is_err(), "it is gone: an unknown id");
    release(h);
    settle(&mut s);
    assert!(s.preview_build.as_ref().is_some_and(|b| Arc::ptr_eq(b, &held)), "no build started after the held one");
}

/// Given builds waiting, when every task is cancelled, then the line is empty and the count includes the waiting ones,
/// so that nothing starts as soon as the running build stops.
#[test]
fn cancelling_every_task_empties_the_line() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    let held = h.build.clone();
    for _ in 0..2 {
        s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap();
    }
    assert_eq!(tasks(&mut s).len(), 3);
    assert_eq!(s.execute("activity.cancel", &json!({"all": true})).unwrap()["cancelled"], 3, "the running one and the two waiting");
    assert_eq!(tasks(&mut s).len(), 1, "only the running (stopping) build is listed");
    release(h);
    settle(&mut s);
    assert!(s.preview_build.as_ref().is_some_and(|b| Arc::ptr_eq(b, &held)), "nothing started after it");
}

/// Given a full line, when one more build is asked for, then it is refused and says why; the line is as it was.
#[test]
fn the_line_is_bounded() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    for place in 1..=MAX_WAITING {
        let r = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap();
        assert_eq!(r["place"].as_u64(), Some(place as u64));
    }
    let e = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap_err().to_string();
    assert!(e.contains(&MAX_WAITING.to_string()), "{e}");
    assert_eq!(tasks(&mut s).len(), MAX_WAITING + 1);
    s.execute("activity.cancel", &json!({"all": true})).unwrap();
    release(h);
}

/// Build Smart Previews in the background waits in the same line (one preview build at a time).
#[test]
fn smart_previews_wait_behind_a_preview_build() {
    let dir = temp_dir("smart");
    let mut s = Session::new();
    s.open_library(&dir, true).unwrap();
    let ids = build_ids(&s, 2);
    let h = hold(&mut s);
    let e = s.execute("library.smartPreviews", &json!({"ids": ids, "background": true})).unwrap_err().to_string();
    assert!(e.contains("already running"), "{e}");
    let r = s.execute("library.smartPreviews", &json!({"ids": ids, "background": true, "queue": true})).unwrap();
    assert!(r["queued"].is_u64(), "{r}");
    let rows = tasks(&mut s);
    assert_eq!(
        (rows[1]["kind"].as_str(), rows[1]["waiting"].as_bool(), rows[1]["lane"].as_str()),
        (Some("smartPreviews"), Some(true), Some("previews"))
    );
    release(h);
    settle(&mut s);
    let progress = s.execute("library.previewProgress", &json!({})).unwrap();
    // (the demo photos aren't files, so it has nothing to build: that it ran at all is what counts)
    assert_eq!(progress["what"], "smart previews", "{progress}");
    assert_eq!(progress["running"], false, "{progress}");
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two builds waiting, the first of which can't start (here: it was asked for a size no build accepts, as if what it
/// needs went away while it waited): the second still runs.
#[test]
fn a_build_that_can_not_start_does_not_hold_up_the_line() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    let bad = crate::activity::Request { command: "library.buildPreviews".into(), params: json!({"size": "huge"}) };
    s.activity.enqueue("previews", "Building previews", bad).unwrap();
    s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap();
    release(h);
    settle(&mut s);
    let progress = s.execute("library.previewProgress", &json!({})).unwrap();
    assert_eq!((progress["total"].as_u64(), progress["done"].as_u64()), (Some(1), Some(1)), "{progress}");
}

/// `queue` with `wait` while a build runs: the call can't both wait in line and return when the build is done.
#[test]
fn a_build_that_would_wait_for_its_turn_and_its_end_is_refused() {
    let mut s = Session::with_demo();
    let h = hold(&mut s);
    let e = s.execute("library.buildPreviews", &json!({"edge": 256, "queue": true, "wait": true})).unwrap_err().to_string();
    assert!(e.contains("wait"), "{e}");
    assert_eq!(tasks(&mut s).len(), 1);
    release(h);
}

/// Given a build waiting and one still running, when another library is opened, then nothing waits any more: the
/// build asked for was about the old library's photos.
#[test]
fn another_library_drops_what_waits() {
    let dir = temp_dir("switch");
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap();
    assert_eq!(tasks(&mut s).len(), 2);
    s.open_library(&dir, false).unwrap();
    let rows = tasks(&mut s);
    assert_eq!(rows.len(), 1, "only the old library's running build: {rows:?}");
    assert_eq!(rows[0]["waiting"], false);
    release(h);
    settle(&mut s);
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
