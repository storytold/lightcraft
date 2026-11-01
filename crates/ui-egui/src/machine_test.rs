//! "Test my machine" in the app (Settings ▸ Performance): the benchmark
//! ([`lightcraft_engine::machine_test`]) runs on a worker thread with a row in the activity stack
//! (✕ stops it); when it ends, its choice (photos side by side, the GPU for exports, exports
//! sharing the GPU) goes into the app's settings, which belong to this install, not a library,
//! and its report is shown under the button.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::machine_test::{MachineReport, TestOptions};

use crate::LightcraftApp;
use crate::state::AppSettings;

/// The test while it runs.
pub struct MachineTask {
    rx: Receiver<Result<MachineReport, String>>,
    /// Its row in the activity stack (its ✕ stops the test).
    _guard: TaskGuard,
    /// Its choice goes into the settings when it ends (`app.testMachine {apply: false}`: it is only
    /// reported).
    apply: bool,
}

/// Make the export settings take effect (lanes, the GPU for exports, the GPU's share).
pub fn apply(st: &AppSettings) {
    lightcraft_engine::export::set_export_lanes(lanes(st));
    lightcraft_engine::export::set_export_gpu(!st.export_cpu_only);
    // (the GPU and the processor at once when the last test found it faster)
    lightcraft_engine::export::set_export_mix(!st.export_cpu_only && st.machine_test.as_ref().is_some_and(|m| m.mix));
    lightcraft_engine::gpu::set_batch_limits(Some(u64::from(st.gpu_export_mb) << 20).filter(|b| *b > 0), gpu_jobs(st));
}

/// What [`apply`] reads from the settings: a change in any of it is applied (the four export
/// settings, and what the last test found, which `ui.set` may change alone).
pub type Tuning = (u32, bool, u32, u32, Option<(usize, bool, usize)>);

/// The settings' [`Tuning`].
pub fn tuning(st: &AppSettings) -> Tuning {
    let tested = st.machine_test.as_ref().map(|m| (m.lanes, m.mix, m.gpu_jobs));
    (st.export_lanes, st.export_cpu_only, st.gpu_export_mb, st.gpu_export_jobs, tested)
}

/// Photos side by side in exports: the number chosen, else (Automatic) what the last test found
/// (note 28: Automatic showed and used half the threads, 12, though the test had found 4 fastest);
/// `None`: the engine's automatic number (no test yet).
pub fn lanes(st: &AppSettings) -> Option<usize> {
    let tested = st.machine_test.as_ref().map(|m| m.lanes).filter(|n| *n > 0);
    Some(st.export_lanes as usize).filter(|n| *n > 0).or(tested)
}

/// Exports sharing the GPU: the number set, else what the last test found.
fn gpu_jobs(st: &AppSettings) -> Option<usize> {
    let tested = st.machine_test.as_ref().map(|m| m.gpu_jobs).filter(|n| *n > 0);
    Some(st.gpu_export_jobs as usize).filter(|j| *j > 0).or(tested)
}

/// Store what a finished test chose in `st`: the export settings go back to Automatic, which from
/// now on is the test's choice (the user's own numbers are replaced by it).
pub fn adopt(st: &mut AppSettings, r: &MachineReport) {
    st.export_lanes = 0;
    st.export_cpu_only = !r.use_gpu && r.gpu.is_some();
    st.gpu_export_jobs = 0;
    st.gpu_export_mb = 0;
    st.machine_test = Some(r.clone());
}

/// Raw photos of the library the test may use, newest last: no file is looked at here, on the UI
/// thread (a library on a slow or gone network share would hold the window); [`on_disk`] picks
/// those that are there, on the test's thread.
fn sample_candidates(app: &LightcraftApp) -> Vec<String> {
    app.session
        .catalog
        .photos()
        .filter(|p| p.kind == lightcraft_engine::catalog::MediaKind::Raw)
        .filter_map(|p| match &p.source {
            lightcraft_engine::catalog::Source::File { path } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// How long the test's thread looks for the library's photos before it uses those it found (and
/// the demo photos when none): a library whose newest folders are on a gone share is not searched
/// for minutes.
const LOOK_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// The newest `n` of `candidates` (newest last) that are files on disk, in their order, looked for
/// newest first until `n` are found or `deadline` passes: the user's own camera's decode is part of
/// an export. Empty: the test uses the demo photos.
fn on_disk(candidates: Vec<String>, n: usize, deadline: std::time::Instant, is_file: impl Fn(&str) -> bool) -> Vec<String> {
    let mut v: Vec<String> = Vec::with_capacity(n);
    for p in candidates.into_iter().rev() {
        if v.len() >= n || std::time::Instant::now() >= deadline {
            break;
        }
        if is_file(&p) {
            v.push(p);
        }
    }
    v.reverse();
    v
}

/// The photos a test runs on: the caller's, or the library's newest (checked on the test's thread).
enum Photos {
    Given(Vec<String>),
    Library(Vec<String>),
}

/// Whether a test is running: the app's, or one a control request (`app.testMachine`) started.
pub fn running(app: &LightcraftApp) -> bool {
    app.machine.is_some() || app.session.activity.list().iter().any(|t| t.kind == "benchmark")
}

/// Start the test (nothing when it already runs or an export is running: it would measure the
/// export too).
pub fn start(app: &mut LightcraftApp) -> Result<(), String> {
    start_with(app, None, false, true)
}

/// `app.testMachine {paths?, quick?, apply?}` in the desktop app: the test of the Settings button,
/// on its thread with its row (✕ stops it), never on the UI thread, which would hold the window
/// for minutes. It answers at once; the report goes into `settings.machineTest` (unless `apply` is
/// false) and the toast, and `ui.inspect` → `machineTest` shows the test running, then how it
/// ended (`last`: the report and whether it was applied, also with `apply: false`).
pub fn command(app: &mut LightcraftApp, p: &serde_json::Value) -> Result<serde_json::Value, String> {
    const C: &str = "app.testMachine";
    let paths: Option<Vec<String>> = match p.get("paths") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Array(a)) => {
            Some(a.iter().map(|v| v.as_str().map(str::to_string).ok_or(format!("{C}: paths must be file paths"))).collect::<Result<_, _>>()?)
        }
        Some(_) => return Err(format!("{C}: paths must be a list of file paths")),
    };
    // (a value that isn't true or false is refused: `"false"` read as absent would apply the test)
    let flag = |k: &str, d: bool| match p.get(k) {
        None | Some(serde_json::Value::Null) => Ok(d),
        Some(v) => v.as_bool().ok_or(format!("{C}: {k} must be true or false")),
    };
    let (quick, apply) = (flag("quick", false)?, flag("apply", true)?);
    start_with(app, paths, quick, apply).map_err(|e| format!("{C}: {e}"))?;
    Ok(
        serde_json::json!({"started": true, "note": "the test runs in the background: ui.inspect → machineTest shows it running, then its report (machineTest.last)"}),
    )
}

fn start_with(app: &mut LightcraftApp, paths: Option<Vec<String>>, quick: bool, apply: bool) -> Result<(), String> {
    if running(app) {
        return Err(crate::i18n::tr("The test is already running").to_string());
    }
    if app.export.is_some() {
        return Err(crate::i18n::tr("Wait for the export to finish, then test this computer").to_string());
    }
    let photos = match paths {
        Some(v) => Photos::Given(v),
        None => Photos::Library(sample_candidates(app)),
    };
    let guard = app.session.activity.start("benchmark", "Testing this computer", Cancel::Yes);
    let cancel = guard.cancel_flag();
    let row = guard.handle();
    let (tx, rx) = channel();
    let stop = cancel.clone();
    let work = move || {
        let paths = match photos {
            Photos::Given(v) => v,
            Photos::Library(c) => on_disk(c, 4, std::time::Instant::now() + LOOK_FOR, |p| std::path::Path::new(p).is_file()),
        };
        let r = run(paths, quick, stop, &mut |done, total, what| {
            row.progress(done as u64, total as u64);
            row.detail(what);
            !cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
        });
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    let started = std::thread::Builder::new().name("machine-test".into()).spawn(work).map(drop).map_err(|e| e.to_string());
    // the web build has no thread to run it on, and run inline it would hold the page (and its
    // Cancel) for minutes: Settings doesn't offer it there
    #[cfg(target_arch = "wasm32")]
    let started: Result<(), String> = {
        drop(work);
        Err(crate::i18n::tr("Test my machine runs in the desktop app").to_string())
    };
    started?;
    app.machine = Some(MachineTask { rx, _guard: guard, apply });
    Ok(())
}

fn run(
    paths: Vec<String>,
    quick: bool,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    progress: &mut dyn FnMut(usize, usize, &str) -> bool,
) -> Result<MachineReport, String> {
    let what = || "the test".to_string();
    lightcraft_engine::guard::catch(&what(), || {
        lightcraft_engine::machine_test::run_test(&TestOptions { paths, quick, cancel, ..Default::default() }, progress)
    })
    .and_then(|r| r)
}

/// When the test has ended: keep and use its choice, and say what it found.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = &app.machine else { return };
    let apply_it = task.apply;
    // (stopped after the test's last look at its flag: its result is not taken)
    let stopped = task._guard.is_cancelled();
    let r = match task.rx.try_recv() {
        Ok(Ok(_)) if stopped => Err("cancelled".to_string()),
        Ok(r) => r,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the test stopped unexpectedly".into()),
    };
    app.machine = None;
    app.machine_last = Some(match &r {
        Ok(report) => serde_json::json!({"applied": apply_it, "report": report}),
        Err(e) => serde_json::json!({"applied": false, "error": e}),
    });
    match r {
        Ok(report) => {
            if apply_it {
                adopt(&mut app.ui.settings, &report);
                apply(&app.ui.settings);
            }
            app.toast_for(ctx, crate::i18n::tr(&report.summary).to_string(), 12.0);
        }
        Err(e) if e == "cancelled" => app.toast(ctx, crate::i18n::tr("Test stopped: the settings are as they were.").to_string()),
        Err(e) => app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Test my machine failed"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_becomes_the_install_settings() {
        let mut st = AppSettings::default();
        let r = MachineReport {
            lanes: 8,
            use_gpu: false,
            gpu: Some("Card".into()),
            gpu_jobs: 2,
            summary: "Exports about 9 photos a minute".into(),
            ..Default::default()
        };
        st.export_lanes = 16;
        adopt(&mut st, &r);
        // Automatic again, and Automatic is now what the test found
        assert_eq!((st.export_lanes, st.export_cpu_only, st.gpu_export_jobs, st.gpu_export_mb), (0, true, 0, 0));
        assert_eq!((lanes(&st), gpu_jobs(&st)), (Some(8), Some(2)));
        // a number the user picks afterwards wins
        st.export_lanes = 6;
        assert_eq!(lanes(&st), Some(6));
        st.export_lanes = 0;
        assert_eq!(st.machine_test.as_ref().map(|m| m.summary.as_str()), Some("Exports about 9 photos a minute"));
        // no GPU at all: not "processor only", which would read as a choice
        adopt(&mut st, &MachineReport { lanes: 4, use_gpu: false, gpu: None, ..Default::default() });
        assert!(!st.export_cpu_only);
        // kept across restarts with the other app settings, and older files without them read
        let json = serde_json::to_value(&st).unwrap();
        let back: AppSettings = serde_json::from_value(json).unwrap();
        assert_eq!(lanes(&back), Some(4));
        let old: AppSettings = serde_json::from_value(serde_json::json!({"gpu": true, "machineTest": "garbage", "exportLanes": "x"})).unwrap();
        assert_eq!((old.export_lanes, old.machine_test.is_none()), (0, true));
    }

    /// Review of PR #3499: a new test report alone (`ui.set` of `settings.machineTest`) changes
    /// what is applied, though the four export settings stay as they were.
    #[test]
    fn a_new_report_alone_is_applied() {
        let mut st = AppSettings::default();
        let before = tuning(&st);
        st.machine_test = Some(MachineReport { lanes: 4, mix: true, gpu_jobs: 2, ..Default::default() });
        assert_ne!(tuning(&st), before);
        let first = tuning(&st);
        if let Some(m) = st.machine_test.as_mut() {
            m.lanes = 6;
        }
        assert_ne!(tuning(&st), first, "another report's lanes");
    }

    /// Review of PR #3499: a test that ends while the full-screen preview is up is applied then,
    /// not when the preview closes.
    #[test]
    fn a_test_that_ends_in_the_full_screen_preview_is_applied() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        h.app.ui.fullscreen = true;
        let (tx, rx) = channel();
        let _guard = h.app.session.activity.start("benchmark", "Testing this computer", Cancel::Yes);
        let _ = tx.send(Ok(MachineReport { lanes: 3, summary: "Exports about 9 photos a minute".into(), ..Default::default() }));
        h.app.machine = Some(MachineTask { rx, _guard, apply: true });
        // (the first frames load the fonts and draw nothing)
        let taken = h.step_until(std::time::Duration::from_secs(20), |h| h.app.machine.is_none());
        assert!(taken && h.app.ui.fullscreen, "the finished test was taken in full screen");
        assert_eq!(h.app.ui.settings.machine_test.as_ref().map(|m| m.lanes), Some(3));
    }

    /// Review of PR #3499: a test that ends while the window is hidden (logic ticks, no drawing)
    /// is taken then, so exports aren't refused for a test that has ended; a test a control
    /// request started counts as running too.
    #[test]
    fn a_test_that_ends_while_hidden_is_taken_by_the_logic_tick() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let ctx = egui::Context::default();
        let (tx, rx) = channel();
        let _guard = app.session.activity.start("benchmark", "Testing this computer", Cancel::Yes);
        let _ = tx.send(Ok(MachineReport { lanes: 3, ..Default::default() }));
        app.machine = Some(MachineTask { rx, _guard, apply: true });
        app.logic(&ctx);
        assert!(app.machine.is_none() && !running(&app), "taken without a drawn frame");
        assert_eq!(app.ui.settings.machine_test.as_ref().map(|m| m.lanes), Some(3));
        let other = app.session.activity.start("benchmark", "Testing this computer", Cancel::Yes);
        assert!(running(&app), "a test started by a control request");
        assert!(start(&mut app).is_err(), "not a second one");
        drop(other);
    }

    /// Review of PR #3506: `app.testMachine` in the desktop app runs the test on its own thread
    /// and answers at once (the window keeps working, ✕ stops it); bad parameters are refused
    /// before anything starts.
    #[test]
    fn a_control_request_tests_off_the_ui_thread() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let ctx = egui::Context::default();
        assert!(command(&mut app, &serde_json::json!({"paths": "x"})).is_err());
        // a flag that isn't true or false is refused, not read as absent ("false" would apply it)
        let e = command(&mut app, &serde_json::json!({"apply": "false"})).unwrap_err();
        assert!(e.contains("apply must be true or false"), "{e}");
        assert!(command(&mut app, &serde_json::json!({"quick": 1})).is_err());
        assert!(!running(&app), "nothing started");
        let t = std::time::Instant::now();
        let r = crate::menus::run_ui_command(&mut app, "app.testMachine", &serde_json::json!({"quick": true, "apply": false}))
            .expect("a UI command")
            .unwrap();
        assert_eq!(r["started"], true, "{r}");
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "answered at once");
        assert!(running(&app) && command(&mut app, &serde_json::json!({})).is_err(), "one test at a time");
        let row = app.session.activity.list().into_iter().find(|t| t.kind == "benchmark").expect("its row");
        let _ = app.session.activity.cancel(row.id);
        let t = std::time::Instant::now();
        while app.machine.is_some() && t.elapsed() < std::time::Duration::from_secs(120) {
            app.logic(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(app.machine.is_none(), "stopped");
        assert!(app.ui.settings.machine_test.is_none(), "a stopped test changes nothing");
        let last = crate::control::inspect(&app, &ctx)["machineTest"].clone();
        assert_eq!((last["running"].as_bool(), last["last"]["applied"].as_bool()), (Some(false), Some(false)), "{last}");
        assert!(last["last"]["error"].is_string(), "{last}");
    }

    /// Review of PR #3506: a test stopped after its last look at its flag, whose report then
    /// arrives, changes nothing.
    #[test]
    fn a_report_that_arrives_after_a_stop_is_not_taken() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let ctx = egui::Context::default();
        let (tx, rx) = channel();
        let _guard = app.session.activity.start("benchmark", "Testing this computer", Cancel::Yes);
        let id = _guard.id();
        app.machine = Some(MachineTask { rx, _guard, apply: true });
        tx.send(Ok(MachineReport { lanes: 5, summary: "Exports about 9 photos a minute".into(), ..Default::default() })).unwrap();
        let _ = app.session.activity.cancel(id);
        poll(&mut app, &ctx);
        assert!(app.machine.is_none());
        assert!(app.ui.settings.machine_test.is_none(), "not taken");
        let last = crate::control::inspect(&app, &ctx)["machineTest"]["last"].clone();
        assert_eq!(last["error"], "cancelled", "{last}");
    }

    /// Review of PR #3499: `app.testMachine {apply: false}` keeps its report where the caller can
    /// read it (`ui.inspect` → `machineTest.last`), and the settings stay as they were.
    #[test]
    fn a_report_that_is_not_applied_is_kept() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let ctx = egui::Context::default();
        let (tx, rx) = channel();
        let _guard = app.session.activity.start("benchmark", "Testing this computer", Cancel::Yes);
        app.machine = Some(MachineTask { rx, _guard, apply: false });
        let report = MachineReport { lanes: 5, gpu_jobs: 2, summary: "Exports about 9 photos a minute".into(), ..Default::default() };
        tx.send(Ok(report.clone())).unwrap();
        poll(&mut app, &ctx);
        assert!(app.machine.is_none());
        assert!(app.ui.settings.machine_test.is_none(), "only reported");
        let last = crate::control::inspect(&app, &ctx)["machineTest"]["last"].clone();
        assert_eq!(last["applied"], false, "{last}");
        assert_eq!(serde_json::from_value::<MachineReport>(last["report"].clone()).ok(), Some(report), "{last}");
    }

    /// Review of PR #3499: the library's photos are looked at on the test's thread, newest first,
    /// and only as many as the test needs.
    #[test]
    fn the_newest_photos_on_disk_are_tested() {
        let c: Vec<String> = (1..=6).map(|i| format!("p{i}.nef")).collect();
        let later = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let looked = std::cell::RefCell::new(Vec::new());
        let gone = |p: &str| {
            looked.borrow_mut().push(p.to_string());
            p != "p5.nef"
        };
        assert_eq!(on_disk(c.clone(), 3, later, gone), ["p3.nef", "p4.nef", "p6.nef"]);
        assert_eq!(*looked.borrow(), ["p6.nef", "p5.nef", "p4.nef", "p3.nef"], "it stops once it has enough");
        assert!(on_disk(c, 4, later, |_| false).is_empty(), "none there: the demo photos");
        // the newest hundred are on a gone share: an older photo is still found
        let many: Vec<String> = (0..200).map(|i| format!("q{i}.nef")).collect();
        assert_eq!(on_disk(many.clone(), 4, later, |p| p == "q7.nef"), ["q7.nef"]);
        // past the deadline: no more looking
        assert!(on_disk(many, 4, std::time::Instant::now(), |_| true).is_empty());
    }

    /// Review of PR #3506: no export, contact sheet or print starts while the test runs (it would
    /// use the test's temporary settings and skew what the test measures).
    #[test]
    fn nothing_exports_while_the_test_runs() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let (_tx, rx) = channel();
        let _guard = app.session.activity.start("benchmark", "Testing this computer", Cancel::Yes);
        app.machine = Some(MachineTask { rx, _guard, apply: true });
        let dir = std::env::temp_dir().join(format!("lc-machine-export-{}", std::process::id()));
        let out = dir.join("out.jpg").to_string_lossy().to_string();
        let pdf = dir.join("out.pdf").to_string_lossy().to_string();
        let e = crate::control::export_active(&mut app, &serde_json::json!({"path": out})).unwrap_err();
        assert_eq!(e, crate::export_task::MACHINE_TEST_RUNNING);
        let e = crate::export_task::start_contact_sheet(&mut app, &serde_json::json!({"path": pdf})).unwrap_err();
        assert_eq!(e, crate::export_task::MACHINE_TEST_RUNNING);
        let e = crate::export_task::start_print(&mut app, &serde_json::json!({"path": pdf})).unwrap_err();
        assert_eq!(e, crate::export_task::MACHINE_TEST_RUNNING);
        assert!(!dir.exists(), "nothing written");
    }
}
