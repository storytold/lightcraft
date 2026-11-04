//! #1015: Help ▸ Report a Bug… writes a report file next to the logs, shows it in the file manager
//! and opens a pre-filled GitHub issue. Nothing is sent; personal paths and photo names are hidden by
//! default.

use crate::headless::Headless;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);

/// A log folder with this run's log and the one before, which name a photo under the home folder.
fn log_dir(tag: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("lc-report-bug-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let home = crate::bug_report::home_dir().unwrap_or_else(|| "/home/someone".into());
    let current = dir.join("lightcraft.log");
    std::fs::write(&current, format!("10:00 INFO start\n10:01 WARN import: {home}/Pictures/Ann birthday.CR2: no preview\n")).unwrap();
    std::fs::write(dir.join("lightcraft.1.log"), "09:00 ERROR the run before\n").unwrap();
    (dir, current.to_string_lossy().to_string())
}

/// The demo app with a log file, recording what it reveals and which links it opens.
fn app(log_file: Option<String>) -> (Headless, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
    let (revealed, opened) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let (r, o) = (revealed.clone(), opened.clone());
    let services = Services {
        png: None,
        log_file,
        reveal: Some(Box::new(move |p: &str| {
            r.lock().unwrap().push(p.to_string());
            Ok(())
        })),
        open_url: Some(Box::new(move |u: &str| {
            o.lock().unwrap().push(u.to_string());
            Ok(())
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1300.0, 900.0], 1.0);
    h.settle(SETTLE);
    (h, revealed, opened)
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

#[test]
fn the_report_file_and_the_issue_hold_what_was_chosen() {
    let (dir, log) = log_dir("create");
    let (mut h, revealed, opened) = app(Some(log));
    let r = h.request("engine.execute", json!({"command": "app.reportBug", "params": {"create": true, "what": "Export hangs at 90 %"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let path = r["result"]["path"].as_str().expect("a report file").to_string();
    assert!(path.starts_with(&dir.to_string_lossy().to_string()) && path.ends_with(".txt"), "{path}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("Export hangs at 90 %") && text.contains("Version: ") && text.contains("== lightcraft.log =="), "{text}");
    // this run's log only, with the home folder and the photo's name hidden
    assert!(!text.contains("lightcraft.1.log") && !text.contains("the run before"), "{text}");
    assert!(text.contains("~/Pictures/<photo>.CR2") && !text.contains("Ann birthday"), "{text}");
    assert_eq!(*revealed.lock().unwrap(), vec![path.clone()]);
    let url = opened.lock().unwrap().last().cloned().expect("the issue opened");
    assert!(
        url.starts_with("https://github.com/storytold/lightcraft/issues/new?title=Bug%3A%20Export%20hangs")
            && url.len() <= crate::bug_report::MAX_URL,
        "{url}"
    );
    assert!(!url.contains("birthday"), "{url}");

    // the previous run's log too, names shown when asked
    let r = h.request("engine.execute", json!({"command": "app.reportBug", "params": {"create": true, "previousLogs": true, "private": false}}), T);
    let text = std::fs::read_to_string(r["result"]["path"].as_str().unwrap()).unwrap();
    assert!(text.contains("== lightcraft.1.log ==") && text.contains("the run before") && text.contains("Ann birthday.CR2"), "{text}");
    assert_eq!(r["result"]["logs"], json!(["lightcraft.log", "lightcraft.1.log"]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_dialog_previews_the_issue_and_creates_the_report() {
    let (dir, log) = log_dir("dialog");
    let (mut h, revealed, opened) = app(Some(log));
    let r = h.request("ui.menu.invoke", json!({"id": "app.reportBug"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::ReportBug { .. })));
    for id in
        ["field:reportBugWhat", "check:reportBugPrivate", "check:reportBugPrevious", "button:reportBugCopy", "button:dialogCancel", "button:dialogOk"]
    {
        assert!(has(&h, id), "no {id}");
    }
    let text = h.painted_text().join("\n");
    assert!(text.contains("Create Report") && text.contains("### What happened") && text.contains("lightcraft.log"), "{text}");
    // the preview hides what the report hides
    assert!(text.contains("<photo>.CR2") && !text.contains("Ann birthday"), "{text}");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none(), "Create Report closes the dialog");
    assert_eq!(revealed.lock().unwrap().len(), 1);
    assert_eq!(opened.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_log_the_issue_holds_the_system_info_only() {
    let (mut h, revealed, opened) = app(None);
    let r = h.request("engine.execute", json!({"command": "app.reportBug", "params": {"create": true}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["result"]["path"].is_null(), "{r}");
    assert!(revealed.lock().unwrap().is_empty());
    let url = opened.lock().unwrap().last().cloned().expect("the issue opened");
    assert!(url.contains("title=Bug%20report") && url.contains("Version%3A%20"), "{url}");
    let r = h.request("ui.menu.invoke", json!({"id": "app.reportBug"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!has(&h, "check:reportBugPrevious"), "no previous logs to offer");
    assert!(h.painted_text().join("\n").contains("keeps no log"));
}
