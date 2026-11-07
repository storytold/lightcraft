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

#[test]
fn logging_public_import_terminal_covers_probe_time_and_private_fields() {
    let thread = capture();
    let base = crate::tests_xmp::temp_dir("PRIVATE-logging-source");
    let path = base.join("PRIVATE-file.jpg");
    std::fs::write(&path, b"synthetic").unwrap();
    let mut s = crate::Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(|_| {
        std::thread::sleep(std::time::Duration::from_millis(30));
        Ok(crate::media::ProbeInfo { width: 32, height: 24, file_size: 9, format: "JPEG".into(), ..Default::default() })
    }));
    let r = s.execute("library.import", &serde_json::json!({"paths":[path],"mode":"add"})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 1);
    let records = rows(thread, "import");
    pair(&records);
    assert_eq!(field(&records[1], "status"), "success");
    assert_eq!(field(&records[1], "persist"), "memory_only");
    assert_eq!(field(&records[1], "imported"), "1");
    assert!(field(&records[1], "elapsed_ms").parse::<u128>().unwrap() >= 20);
    assert!(!records.join(" ").contains("PRIVATE"));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn logging_scan_empty_and_probe_error_each_terminate_once() {
    let thread = capture();
    let mut s = crate::Session::new();
    assert!(super::scan(&mut s, &[]).is_empty());
    let records = rows(thread, "scan");
    pair(&records);
    assert_eq!(field(&records[1], "status"), "success");
    assert_eq!(field(&records[1], "found"), "0");
    let base = crate::tests_xmp::temp_dir("PRIVATE-log-probe");
    let path = base.join("PRIVATE.jpg");
    std::fs::write(&path, b"synthetic").unwrap();
    s.media.file_probe = Some(std::sync::Arc::new(|_| Err("PRIVATE probe error".into())));
    assert!(super::scan(&mut s, &[path.to_string_lossy().into_owned()])[0].error.is_some());
    let records = rows(thread, "scan");
    pair(&records[2..]);
    assert_eq!(field(&records[3], "status"), "error");
    assert_eq!(field(&records[3], "failed"), "1");
    assert!(!records.join(" ").contains("PRIVATE"));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn logging_scan_cancel_waits_for_detached_worker() {
    let thread = capture();
    let base = crate::tests_xmp::temp_dir("PRIVATE-log-cancel");
    let path = base.join("PRIVATE.jpg");
    std::fs::write(&path, b"synthetic").unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let mut s = crate::Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(move |_| {
        entered_tx.send(()).unwrap();
        let _ = release_rx.lock().unwrap().recv();
        Ok(crate::media::ProbeInfo { width: 32, height: 24, ..Default::default() })
    }));
    let paths = vec![path.to_string_lossy().into_owned()];
    let (input, paths) = super::ScanInput::for_import(&mut s, &paths);
    let progress = std::sync::Arc::new(super::ScanProgress::default());
    progress.begin();
    let id = progress.run_id().unwrap().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_progress = progress.clone();
    let worker = std::thread::spawn(move || {
        let _ = tx.send(super::scan_with(input, &paths, &worker_progress));
    });
    entered_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(rx);
    drop(progress);
    let records = || {
        RECORDS
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.starts_with("kind=scan ") && field(s, "run_id") == id)
            .map(|(_, s)| s.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(records().len(), 1, "dropping owner must not emit while probe runs");
    std::thread::sleep(std::time::Duration::from_millis(25));
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    let records = records();
    pair(&records);
    assert_eq!(field(&records[1], "status"), "cancelled");
    assert!(field(&records[1], "elapsed_ms").parse::<u128>().unwrap() >= 20);
    assert!(!records.join(" ").contains("PRIVATE"));
    assert_eq!(rows(thread, "scan").len(), 1, "terminal belongs to worker");
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn logging_scan_delivery_loss_is_cancelled_before_terminal() {
    let thread = capture();
    for accepted in [true, false] {
        let mut s = crate::Session::new();
        let (input, paths) = super::ScanInput::for_import(&mut s, &[]);
        let progress = super::ScanProgress::default();
        progress.begin();
        progress.track_delivery();
        let output = super::scan_with(input, &paths, &progress);
        assert!(output.candidates.is_empty());
        let before = rows(thread, "scan");
        assert_eq!(field(before.last().unwrap(), "event"), "start", "no terminal before delivery outcome");
        progress.delivered(accepted);
        progress.worker_error();
        let records = rows(thread, "scan");
        pair(&records[records.len() - 2..]);
        assert_eq!(field(records.last().unwrap(), "status"), if accepted { "success" } else { "cancelled" });
        assert_eq!(field(records.last().unwrap(), "persist"), "not_applicable");
    }
}

#[test]
fn logging_public_import_probe_failure_is_error_with_numeric_safe_counts() {
    let thread = capture();
    let base = crate::tests_xmp::temp_dir("PRIVATE-logging-command-probe");
    let path = base.join("PRIVATE.jpg");
    std::fs::write(&path, b"synthetic").unwrap();
    let mut s = crate::Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(|_| Err("PRIVATE failure".into())));
    let result = s.execute("library.import", &serde_json::json!({"paths":[path],"mode":"add"})).unwrap();
    assert_eq!(result["failed"].as_array().unwrap().len(), 1);
    let records = rows(thread, "import");
    pair(&records);
    assert_eq!(field(&records[1], "failed"), "1");
    assert_eq!(field(&records[1], "status"), "error");
    assert_eq!(field(&records[1], "cause"), "probe");
    assert!(!records.join(" ").contains("PRIVATE"));
    std::fs::remove_dir_all(base).unwrap();
}
