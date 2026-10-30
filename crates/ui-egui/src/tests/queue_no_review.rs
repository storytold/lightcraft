//! Queue without review: a folder whose review scan runs while the library lane is busy can go straight into the
//! waiting line, as the last review was confirmed, and its row can still be reviewed. Both are links in the activity
//! stack: under the scan's row, and under the waiting row.

use std::time::Duration;

use lightcraft_engine::activity::Cancel;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::import::ImportDialog;
use crate::{LightcraftApp, Services};
use lightcraft_engine::import::ImportCandidate;

const T: Duration = Duration::from_secs(60);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Let the activity stack lay itself out (a running task shows in it once it is half a second old; a waiting one at once).
fn open_list(h: &mut Headless) {
    std::thread::sleep(Duration::from_millis(600));
    for _ in 0..4 {
        h.step();
    }
}

fn click(h: &mut Headless, id: &str) {
    // (the rows moved if the line changed: click where the widget is now)
    h.step();
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    for _ in 0..4 {
        h.step();
    }
}

/// The library lane taken by an import that never ends while the guard lives.
fn busy(h: &Headless) -> lightcraft_engine::activity::TaskGuard {
    h.app.session.activity.start("import", "Importing", Cancel::Yes)
}

/// The request of the one task waiting in line (taking it out of the line, which needs the lane free).
fn waiting_params(h: &Headless) -> Value {
    let w = h.app.session.activity.take_ready().expect("a task waits");
    w.request.params["params"].clone()
}

/// A review confirmed for copying into a folder, as `import::start` asks for it while the lane is taken.
fn confirm_copy(h: &mut Headless, mode_move: bool) {
    let mut d = ImportDialog::new(vec![ImportCandidate { path: "/nowhere/a.jpg".into(), ..Default::default() }]);
    d.copy = true;
    d.move_files = mode_move;
    d.destination = "/Volumes/Photos".into();
    d.organize = "month".into();
    d.keywords = "holiday".into();
    let r = crate::import::start(&mut h.app, &d).unwrap();
    assert!(r.get("queued").is_some(), "it waits: {r}");
    let id = r["queued"].as_u64().unwrap();
    h.app.session.activity.cancel(id).unwrap();
}

/// Given a review scan while the lane is busy, when "Queue without review" is clicked, then the scan stops and the
/// folder waits in line to be added in place (nothing was confirmed before), with the row saying so.
#[test]
fn a_scanned_folder_queues_without_review_while_the_lane_is_busy() {
    let mut h = demo();
    let _lane = busy(&h);
    let _scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    open_list(&mut h);
    click(&mut h, "activity:queueScan");
    assert!(h.app.scan.is_none(), "the scan was let go");
    let waiting = h.app.session.activity.waiting();
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Queued")), "{:?}", h.app.ui.toast);
    assert!(has(&h, &format!("activity:review:{}", waiting[0].id)), "its row can still be reviewed");
    let w = h.app.session.activity.take_ready();
    drop(_lane);
    let w = w.or_else(|| h.app.session.activity.take_ready()).expect("the import waits");
    assert_eq!(w.request.params["params"], json!({"mode": "add"}));
    assert_eq!(w.request.params["paths"], json!(["/nowhere/trip"]));
}

/// Given no scan, or a scan while the lane is free, then there is no link: the review opens as usual.
#[test]
fn there_is_nothing_to_queue_while_the_lane_is_free() {
    let mut h = demo();
    let _scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    open_list(&mut h);
    let id = h.app.session.activity.list().iter().find(|t| t.kind == "scan").expect("its row").id;
    assert!(has(&h, &format!("activity:row:{id}")), "the scan's row is drawn");
    assert!(!has(&h, "activity:queueScan"));
}

/// The Local view's folder read is not a review: it has no link.
#[test]
fn reading_a_folder_for_the_local_view_has_no_link() {
    let mut h = demo();
    let _lane = busy(&h);
    let _scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], true);
    open_list(&mut h);
    let id = h.app.session.activity.list().iter().find(|t| t.kind == "scan").expect("its row").id;
    assert!(has(&h, &format!("activity:row:{id}")), "the scan's row is drawn");
    assert!(!has(&h, "activity:queueScan"));
}

/// Given the last review copied into a folder, then a folder queued without review is copied there the same way,
/// without the review's keywords. Given it moved, then the copy leaves the originals alone.
#[test]
fn a_folder_queued_without_review_follows_the_last_review_but_never_moves() {
    for (moved, mode) in [(false, "copy"), (true, "copy")] {
        let mut h = demo();
        let lane = busy(&h);
        confirm_copy(&mut h, moved);
        let _scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
        open_list(&mut h);
        click(&mut h, "activity:queueScan");
        drop(lane);
        let params = waiting_params(&h);
        assert_eq!(params, json!({"mode": mode, "destination": "/Volumes/Photos", "organize": "month"}), "moved: {moved}");
    }
}

/// Given a folder that was queued without review, when Review… is clicked, then it leaves the line and its review
/// scan starts; while another review scan runs it is refused and stays.
#[test]
fn review_takes_a_queued_folder_out_of_the_line_and_scans_it() {
    let mut h = demo();
    let lane = busy(&h);
    let scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    open_list(&mut h);
    click(&mut h, "activity:queueScan");
    let id = h.app.session.activity.waiting()[0].id;
    // another folder is being scanned: refused, and the row stays
    let other = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/other".into()], false);
    click(&mut h, &format!("activity:review:{id}"));
    assert_eq!(h.app.session.activity.waiting().len(), 1, "the row stays");
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("A folder is already being scanned"));
    drop((other, scan));
    h.app.scan = None;
    // (called, not clicked: the scan of a folder that isn't there ends within the frames a click plays out)
    crate::import::review_queued(&mut h.app, id).unwrap();
    assert!(h.app.session.activity.waiting().is_empty(), "it left the line");
    assert!(h.app.scan.is_some(), "its review scan runs");
    assert!(h.app.unreviewed.is_empty());
    drop(lane);
}

/// Given a folder queued without review that copies into a folder with a long path, then its row still shows
/// Review… inside the list (the long text is cut short, not the link).
#[test]
fn a_long_destination_never_pushes_review_out_of_the_row() {
    let mut h = demo();
    let lane = busy(&h);
    let mut d = ImportDialog::new(vec![ImportCandidate { path: "/nowhere/a.jpg".into(), ..Default::default() }]);
    d.copy = true;
    d.destination = "/Volumes/A very long name of a photo drive/Photos/Originals/Family and friends/Holidays".into();
    let r = crate::import::start(&mut h.app, &d).unwrap();
    h.app.session.activity.cancel(r["queued"].as_u64().unwrap()).unwrap();
    let _scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    open_list(&mut h);
    click(&mut h, "activity:queueScan");
    let id = h.app.session.activity.waiting()[0].id;
    h.step();
    h.step();
    let rect_of = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"));
    let (link, row) = (rect_of(&format!("activity:review:{id}")), rect_of(&format!("activity:row:{id}")));
    assert!(link.right() <= row.right() + 1.0, "{link:?} is past the row's edge {row:?}");
    drop(lane);
}

/// Given a full line, when Queue without review is clicked, then nothing is lost: the scan goes on and its row goes on (the refusal itself is the error `queue_scan` returns).
#[test]
fn a_full_line_leaves_the_scan_running() {
    let mut h = demo();
    let lane = busy(&h);
    let request = lightcraft_engine::activity::Request { command: crate::import::IMPORT_REQUEST.into(), params: json!({"paths": ["/x"]}) };
    while h.app.session.activity.enqueue("import", "Importing", request.clone()).is_ok() {}
    let _scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    let r = crate::import::queue_scan(&mut h.app);
    assert!(r.is_err(), "{r:?}");
    assert!(h.app.scan.is_some(), "the scan was kept");
    assert!(h.app.session.activity.list().iter().any(|t| t.kind == "scan" && !t.cancelling), "its row goes on");
    assert!(h.app.unreviewed.is_empty());
    drop(lane);
}

/// An album an "Import into this album" scan chose (a plain one of the demo library).
fn plain_album(h: &Headless) -> u64 {
    h.app.session.catalog.albums().find(|a| !a.folder && !a.is_smart()).expect("a plain album").id.0
}

/// Given a scan that was started for an album and a card, when it is queued without review, then the import asks for
/// that album, and when Review… is clicked the new scan has the album and the card setting back. Given another library
/// opened since the scan started, the album is not asked for (its number means another album there).
#[test]
fn the_album_and_the_card_of_a_scan_survive_queueing_and_review() {
    let mut h = demo();
    let lane = busy(&h);
    let album = plain_album(&h);
    let scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    if let Some(t) = h.app.scan.as_mut() {
        t.album = Some(album);
        t.copy = true;
    }
    crate::import::queue_scan(&mut h.app).expect("queued");
    drop(scan);
    let id = h.app.session.activity.waiting()[0].id;
    crate::import::review_queued(&mut h.app, id).unwrap();
    let t = h.app.scan.as_ref().expect("its review scan runs");
    assert_eq!((t.album, t.copy), (Some(album), true));
    // the same, with the library replaced since the scan began
    h.app.scan = None;
    let scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    if let Some(t) = h.app.scan.as_mut() {
        t.album = Some(album);
        t.library = t.library.wrapping_add(1);
    }
    crate::import::queue_scan(&mut h.app).expect("queued");
    drop(scan);
    drop(lane);
    assert_eq!(waiting_params(&h)["album"], Value::Null, "no album of another library");
}

/// Given a finished scan's review that waits for a dialog to close, when Review… is called, then it is refused before
/// the folder leaves the line: the row stays.
#[test]
fn review_is_refused_while_another_review_waits_for_its_dialog() {
    let mut h = demo();
    let lane = busy(&h);
    let scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    open_list(&mut h);
    click(&mut h, "activity:queueScan");
    let id = h.app.session.activity.waiting()[0].id;
    drop(scan);
    h.app.review_pending = Some(crate::state::Dialog::Import { opts: Box::new(ImportDialog::new(Vec::new())) });
    let r = crate::import::review_queued(&mut h.app, id);
    assert_eq!(r, Err("Close the open dialog, then choose Review… again".to_string()));
    assert_eq!(h.app.session.activity.waiting().len(), 1, "the row stays");
    assert_eq!(h.app.unreviewed.len(), 1);
    drop(lane);
}

/// Given a review confirmed in one library, when another library is shown, then a folder queued without review there
/// does not follow how that review brought its files in (it is added in place).
#[test]
fn another_library_does_not_inherit_how_the_last_review_brought_files_in() {
    let mut h = demo();
    let lane = busy(&h);
    confirm_copy(&mut h, false);
    assert!(h.app.last_import.is_some());
    h.app.shown_library = Some(u64::MAX);
    h.app.library_changed(&egui::Context::default());
    assert!(h.app.last_import.is_none());
    // a folder queued in the new library is added in place, as with no review before
    let _scan = crate::import::stall_scan_in(&mut h.app, vec!["/nowhere/trip".into()], false);
    open_list(&mut h);
    click(&mut h, "activity:queueScan");
    drop(lane);
    assert_eq!(waiting_params(&h), json!({"mode": "add"}));
}

/// Given a confirmed review that cannot be queued because the line is full, then it is refused and not remembered as
/// the last confirmed review.
#[test]
fn a_review_the_full_line_refused_is_not_remembered() {
    let mut h = demo();
    let _lane = busy(&h);
    let request = lightcraft_engine::activity::Request { command: crate::import::IMPORT_REQUEST.into(), params: json!({"paths": ["/x"]}) };
    while h.app.session.activity.enqueue("import", "Importing", request.clone()).is_ok() {}
    let mut d = ImportDialog::new(vec![ImportCandidate { path: "/nowhere/a.jpg".into(), ..Default::default() }]);
    d.copy = true;
    d.destination = "/Volumes/Photos".into();
    assert!(crate::import::start(&mut h.app, &d).is_err());
    assert!(h.app.last_import.is_none());
}
