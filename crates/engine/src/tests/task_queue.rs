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
use crate::activity::{Cancel, Ending, MAX_WAITING, Request, TaskGuard};
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

/// `activity.finished` rows, newest first.
fn finished(s: &mut Session) -> Vec<Value> {
    s.execute("activity.finished", &json!({})).unwrap()["finished"].as_array().cloned().unwrap_or_default()
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
    let bad = Request { command: "library.buildPreviews".into(), params: json!({"size": "huge"}) };
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

/// Given a build that ended and a build waiting behind it, when the next command starts the waiting one before the
/// app announced the first, then the first is kept (once, with its counts) for the frontend to announce, and the
/// new build is the current one.
#[test]
fn a_build_replaced_by_the_one_waiting_behind_it_is_kept_until_it_is_announced() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    let first = h.build.clone();
    first.done.store(2, Ordering::Relaxed);
    first.failed.store(1, Ordering::Relaxed);
    s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap();
    release(h);
    tasks(&mut s); // the command that ends up starting the waiting build
    assert!(s.preview_build.as_ref().is_some_and(|b| !Arc::ptr_eq(b, &first)), "the waiting build is the current one");
    let kept = s.take_unreported_builds();
    assert_eq!(kept.len(), 1);
    assert!(Arc::ptr_eq(&kept[0], &first));
    assert_eq!((kept[0].done.load(Ordering::Relaxed), kept[0].failed.load(Ordering::Relaxed)), (2, 1));
    assert!(s.take_unreported_builds().is_empty(), "once");
    settle(&mut s);
}

/// A build the frontend announced already is not kept when the next one replaces it.
#[test]
fn a_build_that_was_announced_is_not_kept_when_the_next_one_replaces_it() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    h.build.reported.store(true, Ordering::Relaxed);
    s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap();
    release(h);
    tasks(&mut s);
    assert!(s.take_unreported_builds().is_empty());
    settle(&mut s);
}

/// Given a build that ended and a build waiting in line behind it that has not been started yet, when Build Previews is
/// asked for without `queue`, then it is refused, as it is while a build runs: it does not start ahead of the one
/// waiting, which the line starts after this command.
#[test]
fn a_build_asked_for_without_queue_does_not_go_ahead_of_one_waiting_in_line() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let running = hold(&mut s);
    s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap()["queued"].as_u64().expect("it waits");
    // the build ends, and nothing has started the one waiting yet (no command or frame since)
    release(running);
    let refused = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256})).unwrap_err().to_string();
    assert!(refused.contains("preview build"), "{refused}");
    settle(&mut s);
}

/// Given a build running and two builds waiting behind it, when the build ends, then both start in turn, each one when
/// the lane is free (the line is not "ahead" of the build it is starting).
#[test]
fn builds_waiting_in_line_all_start_in_turn() {
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let running = hold(&mut s);
    for _ in 0..2 {
        s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap()["queued"].as_u64().expect("it waits");
    }
    release(running);
    settle(&mut s);
    // the held build, and the first of the two, were replaced by the next: three builds ran in all
    assert_eq!(s.take_unreported_builds().len(), 2, "both waiting builds started");
}

/// Given a build that can't start and a good one behind it, when the lane frees, then the first leaves a row saying
/// why, which can be retried, and the second still runs.
#[test]
fn a_build_that_could_not_start_says_why_and_the_line_goes_on() {
    let mut s = Session::with_demo();
    s.activity.keep_after(0);
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    // (a request no build accepts: as if what it needs went away while it waited)
    let bad = Request { command: "library.buildPreviews".into(), params: json!({"size": "huge"}) };
    let (bad_id, _) = s.activity.enqueue("previews", "Building previews", bad).unwrap();
    let good = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap()["queued"].as_u64().unwrap();
    release(h);
    settle(&mut s);
    let rows = finished(&mut s);
    let failed = rows.iter().find(|r| r["id"] == bad_id).expect("a row for the build that could not start");
    assert_eq!((failed["ending"].as_str(), failed["retryable"].as_bool()), (Some("failed"), Some(true)));
    assert!(failed["detail"].as_str().is_some_and(|d| d.contains("huge")), "{failed}");
    assert!(rows.iter().any(|r| r["id"] == good && r["ending"] == "done"), "the next one ran: {rows:?}");
}

/// Given a build that could not render a photo (its file is gone), when it is asked for again, then its row goes and
/// the same build runs again (here, it can't render that photo either, and says so in a new row).
#[test]
fn a_build_that_did_not_go_well_can_be_asked_for_again() {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut s = Session::new();
    s.activity.keep_after(0);
    let id = s.catalog.alloc_photo_id();
    let gone = std::env::temp_dir().join(format!("lc-task-queue-gone-{}.jpg", std::process::id()));
    let p = Photo::new(id, Source::File { path: gone.to_string_lossy().to_string() }, "gone.jpg", "JPEG", 600, 400, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.execute("library.buildPreviews", &json!({"ids": [id.0], "edge": 256, "wait": true})).unwrap();
    let row = finished(&mut s).remove(0);
    assert_eq!((row["ending"].as_str(), row["retryable"].as_bool()), (Some("problems"), Some(true)), "{row}");
    let old = row["id"].as_u64().unwrap();
    let r = s.execute("activity.retry", &json!({"id": old})).unwrap();
    let again = r["queued"].as_u64().unwrap();
    settle(&mut s);
    let rows = finished(&mut s);
    assert!(rows.iter().all(|r| r["id"] != old), "the row was taken up by the retry");
    let row = rows.iter().find(|r| r["id"] == again).expect("the retried build's row");
    assert_eq!((row["ending"].as_str(), row["failed"].as_u64()), (Some("problems"), Some(1)), "it built the same photo: {row}");
}

/// Given a smart preview build in the background that did not go well, when it is asked for again, then it is the
/// background smart build that runs again.
#[test]
fn a_smart_preview_build_that_did_not_go_well_can_be_asked_for_again() {
    let mut s = Session::with_demo();
    s.activity.keep_after(0);
    let ids = build_ids(&s, 1);
    let g = s.activity.start("smartPreviews", "Building smart previews", Cancel::Yes);
    g.set_request(Request { command: "library.smartPreviews".into(), params: json!({"ids": ids, "background": true, "discard": true}) });
    let old = g.id();
    g.finish(crate::activity::Outcome::failed("the disk is full"));
    let r = s.execute("activity.retry", &json!({"id": old})).unwrap();
    assert!(r["queued"].as_u64().is_some(), "{r}");
    settle(&mut s);
    assert!(finished(&mut s).iter().all(|f| f["id"] != old), "the row was taken up by the retry");
}

/// Given a build waiting, a failed one that could be retried and one still running (the old library's build goes on),
/// when another library is opened, then nothing waits any more, the failed row stays only as a record, and the running
/// one's row can't be retried when it ends: photo ids belong to the old library.
#[test]
fn another_library_drops_what_could_be_retried() {
    let dir = temp_dir("switch-retry");
    let mut s = Session::with_demo();
    let ids = build_ids(&s, 1);
    let h = hold(&mut s);
    let r = Request { command: "library.buildPreviews".into(), params: json!({"size": "huge"}) };
    let g = s.activity.start("previews", "Building previews", Cancel::Yes);
    g.set_request(r.clone());
    let failed = g.id();
    g.finish(crate::activity::Outcome::failed("no"));
    let running = s.activity.start("previews", "Building previews", Cancel::Yes);
    running.set_request(r);
    let running_id = running.id();
    s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap();
    s.open_library(&dir, false).unwrap();
    assert!(s.activity.waiting().is_empty());
    let row = s.activity.finished().into_iter().find(|f| f.id == failed).expect("the row stays");
    assert!(!row.retryable && row.ending == Ending::Failed);
    assert!(s.execute("activity.retry", &json!({"id": failed})).is_err());
    running.finish(crate::activity::Outcome::failed("stopped after the switch"));
    let row = s.activity.finished().into_iter().find(|f| f.id == running_id).expect("its row");
    assert!(!row.retryable, "it was asked in the old library");
    assert!(s.execute("activity.retry", &json!({"id": running_id})).is_err());
    release(h);
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
