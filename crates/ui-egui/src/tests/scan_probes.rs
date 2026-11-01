//! A scan takes the probes of earlier scans from the session (`ScanInput::new`) and hands them back
//! however it ends: finished, stopped by another folder or by ✕, or unable to list its folder, so a later
//! import doesn't read those files again.

use std::time::Duration;

use crate::LightcraftApp;
use crate::headless::Headless;
use crate::import::ScanEnd;

const T: Duration = Duration::from_secs(10);
const KEPT: &str = "/nowhere/kept.cr3";

fn demo() -> Headless {
    let services = crate::Services { png: None, ..Default::default() };
    Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1000.0, 700.0], 1.0)
}

fn probes() -> std::collections::HashMap<String, lightcraft_engine::media::ProbeInfo> {
    [(KEPT.to_string(), lightcraft_engine::media::ProbeInfo { file_size: 7, ..Default::default() })].into_iter().collect()
}

fn kept(h: &Headless) -> bool {
    h.app.session.import_probes.get(KEPT).is_some_and(|p| p.file_size == 7)
}

fn failed_toast(h: &Headless) -> bool {
    h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("Scan failed"))
}

fn cancel_scan(h: &mut Headless) {
    let id = h.app.session.activity.list().iter().find(|t| t.kind == "scan").map(|t| t.id).expect("the scan's row");
    h.app.session.activity.cancel(id).expect("the scan can be cancelled");
}

#[test]
fn a_browse_replaced_by_another_folder_hands_back_its_probes() {
    let mut h = demo();
    let base = std::env::temp_dir().join(format!("lc-ui-scan-probes-{}", std::process::id()));
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    h.app.session.import_probes = probes();
    crate::import::browse(&mut h.app, &a.to_string_lossy(), None).unwrap();
    assert!(h.app.session.import_probes.is_empty(), "the first browse took the probes");
    // another folder at once: the first browse is stopped (or its result left unread)
    crate::import::browse(&mut h.app, &b.to_string_lossy(), None).unwrap();
    assert!(h.step_until(T, |h| h.app.scan.is_none() && kept(h)), "the first browse's probes came back");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_browse_that_cannot_list_its_folder_hands_back_its_probes() {
    let mut h = demo();
    let tx = crate::import::scan_by_hand(&mut h.app, true);
    tx.send(ScanEnd::Stopped { probes: probes(), failed: true }).unwrap();
    h.step();
    assert!(h.app.scan.is_none());
    assert!(kept(&h), "the probes came back");
    assert!(failed_toast(&h), "the failure is reported: {:?}", h.app.ui.toast);
}

#[test]
fn a_scan_closed_with_its_cross_hands_back_its_probes_when_the_worker_ends() {
    let mut h = demo();
    let tx = crate::import::scan_by_hand(&mut h.app, false);
    cancel_scan(&mut h);
    h.step();
    assert!(h.app.scan.is_none(), "✕ closes the scan at once");
    // the worker ends later, with the probes it took
    tx.send(ScanEnd::Done(lightcraft_engine::import::ScanOutput { candidates: Vec::new(), probes: probes() })).unwrap();
    assert!(h.step_until(T, kept), "the probes came back");
    assert!(h.app.ui.dialog.is_none(), "no review opened");
    assert!(!failed_toast(&h));
}

#[test]
fn a_cancelled_scan_whose_worker_ends_without_a_result_is_not_a_failure() {
    let mut h = demo();
    let tx = crate::import::scan_by_hand(&mut h.app, true);
    cancel_scan(&mut h);
    drop(tx);
    h.step();
    assert!(h.app.scan.is_none());
    assert!(!failed_toast(&h), "a cancelled scan says nothing: {:?}", h.app.ui.toast);
}

/// Browsing reads the disk only on its worker (CodeRabbit on #3077): a folder whose resolution is held (a link on
/// a stalled network mount) still lets `browse` return at once, the view shows the folder as given, and the scan
/// waits; once resolved, the folder is read.
#[test]
fn a_browse_whose_folder_resolves_slowly_returns_at_once_and_waits() {
    let mut h = demo();
    let dir = std::env::temp_dir().join(format!("lc-ui-browse-held-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // (spelled with `sub/..`: the worker is handed the path as given, not the view's spelling of it)
    let given = dir.join("sub").join("..").to_string_lossy().to_string();
    let spelled = lightcraft_engine::import::browse_spelling(&dir).unwrap().to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    crate::import::HELD_RESOLVES.lock().unwrap().push(given.clone());
    let start = std::time::Instant::now();
    let r = crate::import::browse(&mut h.app, &given, None).unwrap();
    assert!(start.elapsed() < Duration::from_secs(2), "returned after {:?}", start.elapsed());
    assert_eq!(r["scanning"], true, "{r}");
    assert_eq!(h.app.session.browse.as_ref().map(|b| b.path.clone()), Some(spelled), "shown as spelled meanwhile");
    for _ in 0..5 {
        h.step();
    }
    assert!(h.app.scan.is_some(), "still waiting for the folder");
    crate::import::HELD_RESOLVES.lock().unwrap().retain(|p| *p != given);
    assert!(h.step_until(T, |h| h.app.scan.is_none()), "read once resolved");
    assert!(!failed_toast(&h), "{:?}", h.app.ui.toast);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A path that isn't a folder is found out by the worker, not by the window's thread: `browse` returns, and the scan
/// then ends as failed.
#[test]
fn a_browse_of_a_missing_folder_fails_on_its_worker() {
    let mut h = demo();
    let gone = std::env::temp_dir().join(format!("lc-ui-browse-gone-{}", std::process::id())).join("not-there");
    assert!(crate::import::browse(&mut h.app, &gone.to_string_lossy(), None).is_ok());
    assert!(h.step_until(T, |h| h.app.scan.is_none()));
    assert!(failed_toast(&h), "the failure is reported: {:?}", h.app.ui.toast);
}

/// The worker resolves the path as given (CodeRabbit on #3055 / #3077 / #3080): on Unix `photos/link/../select`, with
/// `link` pointing at `real/day`, is `real/select`, not the `photos/select` that taking `..` out by name would read.
#[cfg(unix)]
#[test]
fn a_browse_through_a_link_and_dot_dot_reads_the_folder_the_system_names() {
    let mut h = demo();
    let base = std::env::temp_dir().join(format!("lc-ui-browse-link-{}", std::process::id()));
    let (day, real_select, photos) = (base.join("real/day"), base.join("real/select"), base.join("photos"));
    for d in [&day, &real_select, &photos.join("select")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::os::unix::fs::symlink(&day, photos.join("link")).unwrap();
    let given = photos.join("link").join("..").join("select");
    crate::import::browse(&mut h.app, &given.to_string_lossy(), None).unwrap();
    assert!(h.step_until(T, |h| h.app.scan.is_none()), "the browse ended");
    assert!(!failed_toast(&h), "{:?}", h.app.ui.toast);
    let want = std::fs::canonicalize(&real_select).unwrap();
    let shown = h.app.session.browse.as_ref().map(|b| std::fs::canonicalize(&b.path).unwrap());
    assert_eq!(shown, Some(want), "the folder the system names, not photos/select");
    let _ = std::fs::remove_dir_all(&base);
}
