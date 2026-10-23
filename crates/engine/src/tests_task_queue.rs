//! Tasks waiting in line for their lane (see `crate::activity`), with the preview builds.
//!
//! Scenarios, in the words of someone building previews for several shoots:
//!
//! * Build Previews while a build runs: it waits behind it, and the list under the bell says it is next. An agent
//!   asks for that with `queue: true`; without it, the build is refused as before, and says how to wait.
//! * It builds the photos that were chosen when it was asked, not those chosen when its turn comes.
//! * It starts by itself when the build ahead of it ends, under the id it had while it waited.
//! * ✕ on it takes it out of the line: it never runs.
//! * One that could not start says why, and the next one in line still runs.
//! * A build that did not go well can be asked for again, as it was asked.
//! * Another library: nothing waits for it, and nothing of the old one can be asked for again.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use crate::Session;
use crate::activity::{Cancel, Ending, Request, TaskGuard};
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
        let rows = s.execute("activity.list", &json!({})).unwrap()["tasks"].clone();
        if rows.as_array().is_some_and(Vec::is_empty) {
            return;
        }
        assert!(t0.elapsed().as_secs() < 60, "tasks still listed: {rows}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn finished(s: &mut Session) -> Vec<Value> {
    s.execute("activity.finished", &json!({})).unwrap()["finished"].as_array().cloned().unwrap_or_default()
}

/// Given a preview build running and two photos chosen, when Build Previews is asked for with `queue`, then it waits
/// behind the build, first in line. When other photos are chosen meanwhile and the build ahead ends, then it starts
/// by itself under the id it had, and builds the two photos chosen when it was asked.
#[test]
fn a_build_asked_for_while_one_runs_waits_behind_it_and_builds_the_photos_chosen_then() {
    let mut s = Session::with_demo();
    s.activity.keep_after(0);
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    s.execute("library.select", &json!({"ids": [ids[0], ids[1]]})).unwrap();
    let h = hold(&mut s);
    let refused = s.execute("library.buildPreviews", &json!({"edge": 256})).unwrap_err().to_string();
    assert!(refused.contains("already running") && refused.contains("queue"), "{refused}");
    let r = s.execute("library.buildPreviews", &json!({"edge": 256, "queue": true})).unwrap();
    let id = r["queued"].as_u64().expect("an id while it waits");
    assert_eq!(r["place"], 1);
    let rows = s.execute("activity.list", &json!({})).unwrap()["tasks"].clone();
    assert_eq!(rows[1]["id"], id, "{rows}");
    assert_eq!((rows[1]["waiting"].as_bool(), rows[1]["place"].as_u64(), rows[1]["lane"].as_str()), (Some(true), Some(1), Some("previews")));
    assert_eq!(rows[1]["label"], "Building previews");
    s.execute("library.select", &json!({"ids": [ids[2]]})).unwrap();
    release(h);
    settle(&mut s);
    let progress = s.execute("library.previewProgress", &json!({})).unwrap();
    assert_eq!((progress["total"].as_u64(), progress["done"].as_u64()), (Some(2), Some(2)), "{progress}");
    let row = &finished(&mut s)[0];
    assert_eq!((row["id"].as_u64(), row["ending"].as_str()), (Some(id), Some("done")), "the row it leaves has the id it waited under");
    // the journal has the call that asked for it, once: starting it was not a second call
    assert_eq!(s.journal.iter().filter(|(c, _)| c == "library.buildPreviews").count(), 1, "{:?}", s.journal);
}

/// Given a build waiting in line, when the build ahead has ended and its turn has not come yet (the lane is free for
/// the moment), then a new build asked for without `queue` is refused, not started ahead of it, and one with `queue`
/// waits behind it. The one waiting still starts by itself, and then the other is not refused for it.
#[test]
fn a_new_build_does_not_start_ahead_of_one_waiting_while_the_lane_is_free_for_a_moment() {
    let mut s = Session::with_demo();
    s.activity.keep_after(0);
    let ids: Vec<u64> = s.catalog.photos().take(1).map(|p| p.id.0).collect();
    let h = hold(&mut s);
    let held = h.build.clone();
    let waiting = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap()["queued"].as_u64().unwrap();
    // (no command in between: `start_waiting` has not run since the build ended)
    release(h);
    let refused = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256})).unwrap_err().to_string();
    assert!(refused.contains("already running or waiting") && refused.contains("queue"), "{refused}");
    // the refusal was a command like any: it let the waiting build start (it is not held back by its own line)
    assert!(s.preview_build.as_ref().is_some_and(|b| !Arc::ptr_eq(b, &held)), "the build that waited started");
    settle(&mut s);
    assert!(finished(&mut s).iter().any(|r| r["id"] == waiting && r["ending"] == "done"));
}

/// Given a build waiting behind another, when its ✕ is pressed, then it leaves the line and never runs.
#[test]
fn a_waiting_build_taken_out_of_the_line_never_runs() {
    let mut s = Session::with_demo();
    s.activity.keep_after(0);
    let ids: Vec<u64> = s.catalog.photos().take(1).map(|p| p.id.0).collect();
    let h = hold(&mut s);
    let held = h.build.clone();
    let id = s.execute("library.buildPreviews", &json!({"ids": ids, "edge": 256, "queue": true})).unwrap()["queued"].as_u64().unwrap();
    assert_eq!(s.execute("activity.cancel", &json!({"id": id})).unwrap()["cancelled"], 1);
    release(h);
    settle(&mut s);
    assert!(s.preview_build.as_ref().is_some_and(|b| Arc::ptr_eq(b, &held)), "no build started after the held one");
    assert!(finished(&mut s).is_empty(), "nothing ran, nothing to say");
}

/// Given a build that can't start and a good one behind it, when the lane frees, then the first leaves a row saying
/// why, which can be retried, and the second still runs.
#[test]
fn a_build_that_could_not_start_says_why_and_the_line_goes_on() {
    let mut s = Session::with_demo();
    s.activity.keep_after(0);
    let ids: Vec<u64> = s.catalog.photos().take(1).map(|p| p.id.0).collect();
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
    assert!(s.execute("activity.retry", &json!({"id": old})).is_err(), "a row can be retried once");
    assert!(s.execute("activity.retry", &json!({})).is_err());
}

/// Build Smart Previews in the background waits in the same line (one preview build at a time).
#[test]
fn smart_previews_wait_behind_a_preview_build() {
    let dir = temp_dir("smart");
    let mut s = Session::new();
    s.open_library(&dir, true).unwrap();
    s.activity.keep_after(0);
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    let h = hold(&mut s);
    let r = s.execute("library.smartPreviews", &json!({"ids": ids, "background": true, "queue": true})).unwrap();
    let id = r["queued"].as_u64().expect("it waits");
    assert_eq!(s.activity.waiting()[0].kind, "smartPreviews");
    release(h);
    settle(&mut s);
    assert!(finished(&mut s).iter().any(|r| r["id"] == id && r["kind"] == "smartPreviews"));
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `queue` with `wait` while a build runs: the call can't both wait in line and return when the build is done.
#[test]
fn a_build_that_would_wait_for_its_turn_and_its_end_is_refused() {
    let mut s = Session::with_demo();
    let h = hold(&mut s);
    let e = s.execute("library.buildPreviews", &json!({"edge": 256, "queue": true, "wait": true})).unwrap_err().to_string();
    assert!(e.contains("wait"), "{e}");
    assert!(s.activity.waiting().is_empty());
    release(h);
}

/// Given a build waiting, a failed one that could be retried and one still running (the old library's build goes on),
/// when another library is opened, then nothing waits any more, the failed row stays only as a record, and the running
/// one's row can't be retried when it ends: photo ids belong to the old library.
#[test]
fn another_library_drops_what_waits_and_what_could_be_retried() {
    let dir = temp_dir("switch");
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(1).map(|p| p.id.0).collect();
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
