//! The low-disk notice: once per session, when the drive holding the preview cache runs low, with a
//! button to Settings ▸ Performance (where the cache's size limit is).

use std::time::Duration;

use lightcraft_engine::disk_space::DiskSpace;
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const GB: u64 = 1 << 30;

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

#[test]
fn a_low_drive_is_told_once_with_a_way_to_the_performance_settings() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.disk_space_notice = true;
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    assert!(h.app.notices.is_empty(), "no notice before the drive is low");
    // 12 GB free of a 500 GB drive: under 20 GB and under 10 %
    assert!(h.app.session.note_disk_space(DiskSpace { available: 12 * GB, total: 500 * GB }));
    h.step();
    h.step();
    assert_eq!(h.app.notices.len(), 1, "{:?}", h.app.notices);
    let text = h.app.notices[0].clone();
    assert!(text.contains("12.0 GB free of 500 GB") && text.contains("Settings → Performance"), "{text}");
    let inspect = h.request("ui.inspect", json!({}), T);
    assert_eq!(inspect["result"]["notices"][0], text.as_str(), "{inspect}");
    assert!(has(&h, "button:noticeSettings") && has(&h, "button:noticeOk"), "the notice offers the settings");
    let r = h.request("ui.clickWidget", json!({"id": "button:noticeSettings"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.notices.is_empty(), "the button dismisses the notice");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "performance".into() }), "and opens Settings ▸ Performance");
    // lower still, later in the same session: not again
    assert!(!h.app.session.note_disk_space(DiskSpace { available: 2 * GB, total: 500 * GB }));
    h.step();
    h.step();
    assert!(h.app.notices.is_empty(), "once per session");
}

#[test]
fn without_the_desktop_flag_no_notice_is_shown() {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1300.0, 900.0], 1.0);
    assert!(h.app.session.note_disk_space(DiskSpace { available: GB, total: 500 * GB }));
    h.step();
    assert!(h.app.notices.is_empty(), "headless sessions and tests don't show it");
}

/// A notice still queued when another library opens belongs to the library before: it goes, and
/// the new library's drive is judged on its own.
#[test]
fn a_queued_notice_goes_when_another_library_opens() {
    let dir = std::env::temp_dir().join(format!("lc-disk-notice-switch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), Services::default());
    app.disk_space_notice = true;
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    assert!(h.app.session.note_disk_space(DiskSpace { available: GB, total: 500 * GB }));
    h.step();
    assert_eq!(h.app.notices.len(), 1, "{:?}", h.app.notices);
    // another library opens before the notice was answered
    h.app.session.open_library(&dir, false).unwrap();
    h.step();
    h.step();
    assert!(h.app.notices.iter().all(|n| !n.contains("1.0 GB free of 500 GB")), "the previous library's notice is gone: {:?}", h.app.notices);
    let _ = h.app.session.close_library();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The drive is read again every few minutes whatever is filling the cache, not only while
/// previews are built (CodeRabbit on #3080): thumbnails and the loupe's previews alone can take a
/// drive under the line after the library opened.
#[test]
fn a_drive_that_runs_low_without_a_preview_build_is_told() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = std::env::temp_dir().join(format!("lc-disk-notice-later-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let low = std::sync::Arc::new(AtomicBool::new(false));
    let mut session = lightcraft_engine::Session::with_demo().with_fs();
    let l = low.clone();
    session.set_disk_space_reader(Some(std::sync::Arc::new(move |_: &std::path::Path| {
        Some(DiskSpace { available: if l.load(Ordering::Relaxed) { GB } else { 400 * GB }, total: 500 * GB })
    })));
    let mut app = LightcraftApp::new(session, Services::default());
    app.disk_space_notice = true;
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.app.session.open_library(&dir, false).unwrap();
    // (the open's read answered: a pass before that would see no alert yet whatever the drive holds)
    assert!(h.step_until(T, |h| !h.app.session.disk_space_reading() && h.app.session.preview_build.is_none()));
    assert!(!h.app.session.disk_space_alert_raised());
    for _ in 0..5 {
        h.step();
    }
    assert!(h.app.notices.is_empty(), "plenty of space at open: {:?}", h.app.notices);
    // later, the drive fills up while nothing builds previews
    low.store(true, Ordering::Relaxed);
    h.app.session.forget_disk_space_check();
    assert!(h.app.session.preview_build.as_ref().is_none_or(|b| !b.running()));
    assert!(h.step_until(T, |h| !h.app.notices.is_empty()), "the low drive is told");
    assert!(h.app.notices[0].contains("1.0 GB free of 500 GB"), "{:?}", h.app.notices);
    let _ = h.app.session.close_library();
    let _ = std::fs::remove_dir_all(&dir);
}

/// An idle window still takes the drive's answer and makes the next check (CodeRabbit on #3077):
/// while a read runs the notice logic asks for a pass soon, and otherwise for one when the next
/// check is due; once the alert was raised it asks for nothing.
#[test]
fn an_idle_window_wakes_for_the_disk_check() {
    use std::sync::{Arc, Mutex, mpsc};
    let dir = std::env::temp_dir().join(format!("lc-disk-notice-idle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (release, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let mut session = lightcraft_engine::Session::with_demo().with_fs();
    session.set_disk_space_reader(Some(Arc::new(move |_: &std::path::Path| {
        let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(10));
        Some(DiskSpace { available: GB, total: 500 * GB })
    })));
    let mut app = LightcraftApp::new(session, Services::default());
    app.disk_space_notice = true;
    app.session.open_library(&dir, false).unwrap();
    assert!(app.session.disk_space_reading());
    let ctx = egui::Context::default();
    let pass = |app: &mut LightcraftApp| {
        let mut out = ctx.run_ui(egui::RawInput::default(), |_| crate::panels::notices::logic(app, &ctx));
        out.textures_delta.clear();
        out.viewport_output.values().map(|v| v.repaint_delay).min().unwrap_or(Duration::MAX)
    };
    assert!(pass(&mut app) <= Duration::from_millis(250), "a pass soon while the read runs");
    release.send(()).unwrap();
    let until = std::time::Instant::now() + T;
    while app.notices.is_empty() {
        assert!(std::time::Instant::now() < until, "the answer was never taken");
        pass(&mut app);
    }
    assert!(pass(&mut app) > Duration::from_secs(60), "raised: nothing more to wake for");
    let _ = app.session.close_library();
    let _ = std::fs::remove_dir_all(&dir);
}
