//! Slow or offline drives never stall a frame (issue #104): with an injected file-system probe
//! that does not answer at all until the test lets it (a sleeping NAS), the grid, the Info panel,
//! the Rename dialog, the Missing Photos view and an import keep drawing frames, the probe never
//! runs on the UI thread, and the background answers arrive once the drive wakes up.
//!
//! The NAS is a gate, not a sleep: a frame that waited for the drive would wait for good (until a
//! watchdog gives up and records it), so the tests need no frame-time budget and hold on a slow
//! or loaded machine (the FreeBSD CI VM) as they do on a fast one.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::{LibrarySource, Session};
use serde_json::json;

use crate::headless::Headless;
use crate::state::{RightPanel, ViewMode};
use crate::{LightcraftApp, Services};

const N: u64 = 12;
/// How long the drive stays asleep at most: only a frame stuck waiting for it gets this far.
const WATCHDOG: Duration = Duration::from_secs(120);

/// Threads the probe ran on, and how often.
#[derive(Clone, Default)]
struct Calls {
    threads: Arc<Mutex<Vec<ThreadId>>>,
    n: Arc<AtomicUsize>,
}

impl Calls {
    fn record(&self) {
        self.threads.lock().unwrap().push(std::thread::current().id());
        self.n.fetch_add(1, Ordering::SeqCst);
    }
    fn on(&self, t: ThreadId) -> usize {
        self.threads.lock().unwrap().iter().filter(|x| **x == t).count()
    }
}

/// The sleeping drive: every file-system call waits until [`Nas::wake`] (or takes one of the
/// calls [`Nas::let_through`] allows). A watchdog wakes it after [`WATCHDOG`] and records that it
/// had to (some frame was waiting for the drive).
#[derive(Default)]
struct Nas {
    /// (awake, calls let through while asleep)
    state: Mutex<(bool, usize)>,
    changed: Condvar,
    forced: AtomicBool,
}

impl Nas {
    fn asleep() -> Arc<Self> {
        let nas = Arc::new(Nas::default());
        let dog = nas.clone();
        std::thread::spawn(move || {
            let state = dog.state.lock().unwrap();
            let (mut state, _) = dog.changed.wait_timeout_while(state, WATCHDOG, |s| !s.0).unwrap();
            if !state.0 {
                dog.forced.store(true, Ordering::SeqCst);
                state.0 = true;
                dog.changed.notify_all();
            }
        });
        nas
    }
    fn wait(&self) {
        let state = self.state.lock().unwrap();
        let mut state = self.changed.wait_while(state, |s| !s.0 && s.1 == 0).unwrap();
        if !state.0 {
            state.1 -= 1;
        }
    }
    /// Answer `n` more calls, then sleep again.
    fn let_through(&self, n: usize) {
        self.state.lock().unwrap().1 += n;
        self.changed.notify_all();
    }
    fn wake(&self) {
        self.state.lock().unwrap().0 = true;
        self.changed.notify_all();
    }
    /// The watchdog had to wake the drive: a frame waited for it.
    fn forced(&self) -> bool {
        self.forced.load(Ordering::SeqCst)
    }
}

/// `N` photos whose originals are on an unreachable share, a smart previews folder, and a probe
/// that waits for the NAS to wake up before saying "not there". Called on the UI thread: a call
/// from that thread is recorded and answered at once, so a regression fails the thread check
/// instead of hanging the test.
fn offline_library(calls: &Calls, nas: &Arc<Nas>) -> Session {
    let mut s = Session::new();
    for i in 0..N {
        let path = format!("/lightcraft-offline-nas/IMG_{i:03}.jpg");
        let p = Photo::new(PhotoId(i + 1), Source::File { path }, &format!("IMG_{i:03}.jpg"), "JPEG", 600, 400, "2026-01-01T00:00:00");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s.media.smart_dir = Some(std::env::temp_dir().join("lightcraft-offline-smart-previews-none"));
    let (c, nas, ui_thread) = (calls.clone(), nas.clone(), std::thread::current().id());
    s.media.availability.set_probe(Arc::new(move |_| {
        c.record();
        if std::thread::current().id() != ui_thread {
            nas.wait();
        }
        false
    }));
    s
}

/// Run `frames` frames while the drive sleeps; each one has to finish without it.
fn frames_while_asleep(h: &mut Headless, nas: &Nas, frames: usize, what: &str) {
    for _ in 0..frames {
        h.step();
        assert!(!nas.forced(), "a {what} frame waited for the sleeping drive");
    }
}

#[test]
fn offline_originals_never_block_a_frame() {
    let calls = Calls::default();
    let nas = Nas::asleep();
    let app = LightcraftApp::new(offline_library(&calls, &nas), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let ui_thread = std::thread::current().id();
    h.app.ui.view = ViewMode::PhotoGrid;
    h.step();
    // The first frame installs fonts; finish the first layout before the drive matters.
    h.step();
    assert!(h.app.fonts_ready, "the UI is initialized");
    assert!(h.app.session.media.availability.is_background(), "the app checks files off the UI thread");

    // the grid: every cell asks for its thumbnail (which asks whether the original is there)
    h.app.session.execute("library.select", &json!({"ids": [1]})).unwrap();
    frames_while_asleep(&mut h, &nas, 20, "grid");

    // the Info panel of the active photo, in the loupe
    h.app.ui.view = ViewMode::Detail;
    h.app.ui.right = RightPanel::Info;
    frames_while_asleep(&mut h, &nas, 20, "Info panel");

    // the Rename dialog's preview (it checks which names are taken on disk)
    h.app.ui.view = ViewMode::PhotoGrid;
    h.app.ui.right = RightPanel::None;
    h.app.session.execute("library.select", &json!({"ids": [1, 2, 3]})).unwrap();
    h.app.ui.dialog = Some(crate::state::Dialog::Rename { template: "Trip-{seq:3}".into(), start: 1 });
    frames_while_asleep(&mut h, &nas, 20, "Rename dialog");
    h.app.ui.dialog = None;

    // the Missing Photos view: nothing is known yet, so nothing is listed, and frames go on
    h.app.session.execute("library.source", &json!({"kind": "missing"})).unwrap();
    assert_eq!(h.app.session.source, LibrarySource::Missing);
    frames_while_asleep(&mut h, &nas, 20, "Missing Photos");
    // the probe runs on the background worker: give it time to start on a loaded machine
    let asked = std::time::Instant::now();
    while calls.n.load(Ordering::SeqCst) == 0 && asked.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(calls.n.load(Ordering::SeqCst) > 0, "the probe was asked");
    assert_eq!(h.app.session.visible().len(), 0, "unknown files count as there");

    // the drive answers: the view fills in as the background checks find the files gone
    nas.wake();
    let t0 = Instant::now();
    while h.app.session.visible().len() < N as usize && t0.elapsed() < Duration::from_secs(60) {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.app.session.visible().len(), N as usize, "every photo found missing in the end");
    assert_eq!(calls.on(ui_thread), 0, "the slow probe never ran on the UI thread");
}

/// An import whose files take long to read (probe: decoding headers and hashing on a slow drive)
/// runs on a worker thread: frames go on while the drive holds every read, the probe never runs
/// on the UI thread, and Cancel stops it with what was read so far added.
#[test]
fn import_reads_files_off_the_ui_thread_and_can_be_cancelled() {
    let dir = std::env::temp_dir().join(format!("lc-ui-slow-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let n = 120;
    for i in 0..n {
        std::fs::write(dir.join(format!("IMG_{i:03}.jpg")), format!("not really a jpeg {i}")).unwrap();
    }
    let calls = Calls::default();
    let nas = Nas::asleep();
    let mut s = Session::new();
    let (c, drive, ui_thread) = (calls.clone(), nas.clone(), std::thread::current().id());
    s.media.file_probe = Some(Arc::new(move |p: &str| {
        c.record();
        // on the UI thread: answered at once, so a regression fails the thread check, not by hanging
        if std::thread::current().id() != ui_thread {
            drive.wait();
        }
        Ok(lightcraft_engine::media::ProbeInfo {
            width: 60,
            height: 40,
            format: "JPEG".into(),
            content_hash: Some(p.to_string()),
            ..Default::default()
        })
    }));
    let app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.step();
    // The first frame installs fonts; finish the first layout before the drive matters.
    h.step();
    assert!(h.app.fonts_ready, "the UI is initialized");
    let undo0 = h.app.session.undo.len();
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    // every read is held: frames go on, and the import's row shows (after 0.5 s) with its ✕
    frames_while_asleep(&mut h, &nas, 20, "import");
    assert!(h.app.import.is_some(), "importing");
    let id = h.app.session.activity.list().first().map(|t| t.id).expect("an import row");
    let cross = format!("activity:cancel:{id}");
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.widgets.iter().any(|(w, _)| *w == cross)), "the row shows");
    assert!(!nas.forced(), "an import frame waited for the sleeping drive");
    // the drive answers one batch of reads: it joins the catalog while the next batch waits
    let batch = lightcraft_engine::import::batch_size();
    nas.let_through(batch);
    let t0 = Instant::now();
    while h.app.import.as_ref().is_some_and(|t| t.imported < batch) && t0.elapsed() < Duration::from_secs(60) {
        h.step();
        assert!(!nas.forced(), "an import frame waited for the sleeping drive");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(h.app.import.is_some(), "still importing");
    let status = h.app.import.as_ref().map(crate::import::ImportTask::status).unwrap_or_default();
    assert!(status["imported"].as_u64().unwrap_or(0) >= 8 && status["total"].as_u64().unwrap_or(0) >= 8, "{status}");
    // Cancel (the ✕ of its row in the activity stack): the files being read finish, nothing new starts
    let r = h.request("ui.clickWidget", json!({"id": cross}), Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    nas.wake();
    let t0 = Instant::now();
    while h.app.import.is_some() && t0.elapsed() < Duration::from_secs(60) {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(h.app.import.is_none(), "stopped");
    let added = h.app.session.catalog.len();
    assert!((8..n).contains(&added), "cancelled part-way: {added} of {n} added");
    assert_eq!(h.app.session.undo.len(), undo0 + 1, "one undo step");
    assert_eq!(calls.on(ui_thread), 0, "files were never read on the UI thread");
    let _ = std::fs::remove_dir_all(&dir);
}
