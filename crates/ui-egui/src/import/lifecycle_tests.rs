use std::sync::{Mutex, Once};

static RECORDS: Mutex<Vec<(std::thread::ThreadId, String)>> = Mutex::new(Vec::new());
static INIT: Once = Once::new();
struct Capture;
impl log::Log for Capture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "lightcraft::import"
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            RECORDS.lock().unwrap().push((std::thread::current().id(), record.args().to_string()));
        }
    }
    fn flush(&self) {}
}
pub(crate) fn capture() -> std::thread::ThreadId {
    INIT.call_once(|| {
        log::set_logger(&Capture).unwrap();
        log::set_max_level(log::LevelFilter::Info);
    });
    std::thread::current().id()
}
pub(crate) fn rows(thread: std::thread::ThreadId, kind: &str) -> Vec<String> {
    RECORDS.lock().unwrap().iter().filter(|(id, s)| *id == thread && s.starts_with(&format!("kind={kind} "))).map(|(_, s)| s.clone()).collect()
}
fn field<'a>(record: &'a str, key: &str) -> &'a str {
    record
        .split_whitespace()
        .filter_map(|v| v.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("missing {key}: {record}"))
}
fn pair(records: &[String]) {
    assert_eq!(records.len(), 2, "one start and one terminal: {records:?}");
    assert_eq!(field(&records[0], "event"), "start");
    assert_eq!(field(&records[1], "event"), "terminal");
    assert_eq!(field(&records[0], "run_id"), field(&records[1], "run_id"));
}

fn fixture(tag: &str, n: usize) -> (crate::LightcraftApp, Vec<String>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-logging-{tag}-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let mut paths = Vec::new();
    for i in 0..n {
        let p = dir.join(format!("PRIVATE-{i}.jpg"));
        std::fs::write(&p, format!("synthetic{i}")).unwrap();
        paths.push(p.to_string_lossy().into_owned());
    }
    let mut s = lightcraft_engine::Session::new();
    s.open_library(dir.join("library"), false).unwrap();
    s.media.file_probe = Some(std::sync::Arc::new(|p| {
        Ok(lightcraft_engine::media::ProbeInfo {
            width: 32,
            height: 24,
            format: "JPEG".into(),
            content_hash: Some(p.to_owned()),
            ..Default::default()
        })
    }));
    (crate::LightcraftApp::new(s, crate::Services { png: None, ..Default::default() }), paths, dir)
}
fn finish_task(app: &mut crate::LightcraftApp, ctx: &egui::Context) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while app.import.is_some() && std::time::Instant::now() < end {
        super::tick(app, ctx);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(app.import.is_none(), "import must finish");
}
#[test]
fn logging_desktop_multibatch_and_early_returns_have_one_persisted_terminal() {
    let thread = capture();
    let ctx = egui::Context::default();
    for (i, mode) in ["normal", "auto", "local"].into_iter().enumerate() {
        let (mut app, paths, dir) = fixture(mode, 33);
        let mut task = super::ImportTask::new(paths, serde_json::json!({"mode":"add","local":mode=="local"}), 0, mode == "local");
        if mode == "auto" {
            task = task.auto();
        }
        app.import = Some(task);
        finish_task(&mut app, &ctx);
        assert_eq!(app.session.catalog.len(), 33);
        let records = rows(thread, "import");
        pair(&records[i * 2..]);
        let terminal = &records[i * 2 + 1];
        assert_eq!(field(terminal, "status"), "success");
        assert_eq!(field(terminal, "imported"), "33");
        assert_eq!(field(terminal, "persist"), "ok");
        assert!(!records.join(" ").contains("PRIVATE"));
        drop(app);
        let mut s = lightcraft_engine::Session::new();
        s.open_library(dir.join("library"), false).unwrap();
        assert_eq!(s.catalog.len(), 33);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn logging_desktop_worker_error_is_not_successful_channel_exhaustion() {
    let thread = capture();
    let (mut app, paths, dir) = fixture("worker-error", 1);
    app.session.media.file_probe = Some(std::sync::Arc::new(|_| panic!("PRIVATE worker panic")));
    app.import = Some(super::ImportTask::new(paths, serde_json::json!({"mode":"add"}), 0, false));
    finish_task(&mut app, &egui::Context::default());
    let records = rows(thread, "import");
    pair(&records);
    assert_eq!(field(&records[1], "status"), "error");
    assert_eq!(field(&records[1], "cause"), "worker");
    assert!(!records.join(" ").contains("PRIVATE"));
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

fn run_rows(id: u64) -> Vec<String> {
    RECORDS
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, s)| s.starts_with("kind=import ") && field(s, "run_id") == id.to_string())
        .map(|(_, s)| s.clone())
        .collect()
}
fn wait_done(completion: &std::sync::Arc<Mutex<super::ImportCompletion>>) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !completion.lock().unwrap().worker_done && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(completion.lock().unwrap().worker_done);
}
#[test]
fn logging_desktop_detach_waits_for_worker_in_both_orders() {
    capture();
    for worker_first in [false, true] {
        let (mut app, paths, dir) = fixture(if worker_first { "detach-worker-first" } else { "detach-owner-first" }, 3);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let first = std::sync::atomic::AtomicBool::new(true);
        app.session.media.file_probe = Some(std::sync::Arc::new(move |_| {
            if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
                entered_tx.send(()).unwrap();
                let _ = release_rx.lock().unwrap().recv();
            }
            Ok(lightcraft_engine::media::ProbeInfo { width: 32, height: 24, ..Default::default() })
        }));
        let task = super::ImportTask::new(vec![dir.to_string_lossy().into_owned()], serde_json::json!({"mode":"add"}), 0, false);
        let _ = paths;
        let completion = task.completion.clone();
        let id = completion.lock().unwrap().id;
        app.import = Some(task);
        super::tick(&mut app, &egui::Context::default());
        entered_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let final_done = app.import.as_ref().unwrap().run.as_ref().unwrap().done.clone();
        if worker_first {
            release_tx.send(()).unwrap();
            wait_done(&completion);
            assert_eq!(run_rows(id).len(), 1, "worker alone cannot claim persisted completion");
            drop(app.import.take());
        } else {
            drop(app.import.take());
            assert_eq!(run_rows(id).iter().filter(|s| field(s, "event") == "terminal").count(), 0);
            std::thread::sleep(std::time::Duration::from_millis(25));
            release_tx.send(()).unwrap();
            wait_done(&completion);
        }
        let records = run_rows(id);
        assert_eq!(records.len(), 3, "start/request/terminal once: {records:?}");
        assert_eq!(field(&records[1], "event"), "cancel_requested");
        assert_eq!(field(&records[2], "event"), "terminal");
        assert_eq!(field(&records[2], "status"), "cancelled");
        assert_eq!(field(&records[2], "persist"), "unknown");
        assert_eq!(field(&records[2], "owner_detached"), "true");
        assert_eq!(
            field(&records[2], "done").parse::<usize>().unwrap(),
            final_done.load(std::sync::atomic::Ordering::Relaxed),
            "final worker counters, not owner-drop snapshot"
        );
        assert_eq!(field(&records[2], "total"), "3", "expanded folder count, not one requested folder");
        assert_eq!(field(&records[2], "imported"), "0", "uncommitted prepared results stay uncommitted");
        assert!(!records.join(" ").contains("PRIVATE"));
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn logging_desktop_cancel_after_persisted_batch_retains_counts() {
    let thread = capture();
    let (mut app, paths, dir) = fixture("partial-cancel", 33);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let count = std::sync::atomic::AtomicUsize::new(0);
    app.session.media.file_probe = Some(std::sync::Arc::new(move |p| {
        if count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == lightcraft_engine::import::batch_size() {
            entered_tx.send(()).unwrap();
            let _ = release_rx.lock().unwrap().recv();
        }
        Ok(lightcraft_engine::media::ProbeInfo {
            width: 32,
            height: 24,
            format: "JPEG".into(),
            content_hash: Some(p.to_owned()),
            ..Default::default()
        })
    }));
    app.import = Some(super::ImportTask::new(paths, serde_json::json!({"mode":"add"}), 0, false));
    let ctx = egui::Context::default();
    super::tick(&mut app, &ctx);
    entered_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    super::tick(&mut app, &ctx);
    let committed = app.session.catalog.len();
    assert_eq!(committed, lightcraft_engine::import::batch_size());
    let task = app.import.as_mut().unwrap();
    task.run.as_ref().unwrap().cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    task.cancelled = true;
    assert_eq!(rows(thread, "import").len(), 1);
    release_tx.send(()).unwrap();
    finish_task(&mut app, &ctx);
    let records = rows(thread, "import");
    pair(&records);
    assert_eq!(field(&records[1], "status"), "cancelled");
    assert_eq!(field(&records[1], "persist"), "ok");
    assert!(field(&records[1], "imported").parse::<usize>().unwrap() >= committed);
    let actual = app.session.catalog.len();
    assert_eq!(field(&records[1], "imported").parse::<usize>().unwrap(), actual);
    drop(app);
    let mut reopened = lightcraft_engine::Session::new();
    reopened.open_library(dir.join("library"), false).unwrap();
    assert_eq!(reopened.catalog.len(), actual);
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn logging_desktop_scan_owner_drop_cancels_paused_computation() {
    let thread = capture();
    let (mut app, paths, dir) = fixture("scan-detached", 1);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    app.session.media.file_probe = Some(std::sync::Arc::new(move |_| {
        entered_tx.send(()).unwrap();
        let _ = release_rx.lock().unwrap().recv();
        Ok(lightcraft_engine::media::ProbeInfo { width: 32, height: 24, ..Default::default() })
    }));
    super::open(&mut app, paths).unwrap();
    entered_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let start = rows(thread, "scan").pop().unwrap();
    let id = field(&start, "run_id").to_owned();
    drop(app.scan.take());
    let records = || {
        RECORDS
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.starts_with("kind=scan ") && field(s, "run_id") == id)
            .map(|(_, s)| s.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(records().len(), 1);
    release_tx.send(()).unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while records().len() < 2 && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let records = records();
    pair(&records);
    assert_eq!(field(&records[1], "status"), "cancelled");
    assert_eq!(field(&records[1], "cause"), "cancelled");
    assert!(!records.join(" ").contains("PRIVATE"));
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}
